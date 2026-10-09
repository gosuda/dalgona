//! The `search` tool: the argument shell and the `find`, `grep`, and `symbol` modes.
//!
//! The shell decodes strict arguments, publishes the schema for the current
//! `search_symbols` flag, resolves the path scope, dispatches to a mode, calls
//! the optional reranker after deterministic ranking, and assembles notes.

pub(crate) mod find;
mod grep;
pub(crate) mod index;
mod page;
#[cfg(feature = "symbols")]
mod symbol;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dal_agent::error::ToolError;
use dal_agent::ext::{
    BoxFuture, Services, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput, tool::ArgError,
};
use dal_core::{
    Consumer, FindEntry, FindPage, GenerationId, ModelInfo, Name, RawJson, RegistrationError,
    SessionId, SymbolPage, ToolClass, ToolData, ToolSpec, TurnId, Workspace,
};
use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};

use crate::evidence::Binding;
use crate::patch::snapshot::SnapshotStore;
use crate::{Rerank, RerankCall, RerankCandidate, Seen};
use find::{ContentClass, Entry, FindGlob, FindResult};
use grep::SeenRow;
use index::Index;
use page::Page;

/// Schema published while symbol mode is off, exact bytes.
const SCHEMA_OFF: &str = r#"{"type":"object","properties":{"mode":{"type":"string","enum":["find","grep"]},"pattern":{"type":"string","description":"find: a glob. grep: a regular expression, or plain text when literal is true."},"path":{"type":"string","description":"Directory or file to search. Default: the workspace root."},"glob":{"type":"string","description":"grep only: search only files whose path matches this glob."},"literal":{"type":"boolean","description":"grep only: treat pattern as plain text. Default false."},"ignore_case":{"type":"boolean","description":"Default false."},"limit":{"type":"integer","minimum":1,"maximum":1000,"description":"find: paths. grep: matches. Default 200."}},"required":["mode","pattern"],"additionalProperties":false}"#;

/// Schema published while symbol mode is on, exact bytes.
const SCHEMA_ON: &str = r#"{"type":"object","properties":{"mode":{"type":"string","enum":["find","grep","symbol"]},"pattern":{"type":"string","description":"find: a glob. grep: a regular expression, or plain text when literal is true. symbol: a name or an A::b path, [2] for the second of equal names, or * with path set to one file."},"path":{"type":"string","description":"Directory or file to search. Default: the workspace root."},"glob":{"type":"string","description":"grep only: search only files whose path matches this glob."},"literal":{"type":"boolean","description":"grep only: treat pattern as plain text. Default false."},"ignore_case":{"type":"boolean","description":"Default false."},"limit":{"type":"integer","minimum":1,"maximum":1000,"description":"find: paths. grep: matches. Default 200."}},"required":["mode","pattern"],"additionalProperties":false}"#;

/// Description published while symbol mode is off, exact bytes.
const DESCRIPTION_OFF: &str = "Search the workspace. mode \"find\" lists files whose path matches the glob in pattern. A glob without / matches the file name at any depth. mode \"grep\" lists lines that match pattern, a regular expression in Rust regex syntax without backreferences or lookaround, or plain text when literal is true. Search skips .git and every path that .gitignore or .git/info/exclude excludes. It does not follow symbolic links. It skips binary files and files over 16 MiB. find returns at most limit paths, sorted. grep returns at most limit matches as path:line:text and cuts lines at 500 characters. When results were cut, the last line starts with [Truncated:.";

/// Description published while symbol mode is on, exact bytes.
const DESCRIPTION_ON: &str = "Search the workspace. mode \"find\" lists files whose path matches the glob in pattern. A glob without / matches the file name at any depth. mode \"grep\" lists lines that match pattern, a regular expression in Rust regex syntax without backreferences or lookaround, or plain text when literal is true. Search skips .git and every path that .gitignore or .git/info/exclude excludes. It does not follow symbolic links. It skips binary files and files over 16 MiB. find returns at most limit paths, sorted. grep returns at most limit matches as path:line:text and cuts lines at 500 characters. When results were cut, the last line starts with [Truncated:. mode \"symbol\" lists definitions whose name ends with pattern as <path>:<first>-<last> <kind> <name>. When exactly one definition matches, it is shown whole with its tag, which patch needs. pattern * with path set to one file lists that file's definitions.";

/// The error for symbol mode while `search_symbols` is off or the feature is compiled out.
const SYMBOL_OFF: &str =
    "search: mode \"symbol\" is off. Set search_symbols = true in config.toml.";
const MODE_ERROR: &str = "search: mode must be \"find\", \"grep\", or \"symbol\"";
const EMPTY_PATTERN: &str = "search: pattern must not be empty";
const LIMIT_ERROR: &str = "search: limit must be between 1 and 1000";
const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: u64 = 1000;
/// The most find rows gathered for a reranker before the `limit` cut.
const RERANK_WINDOW: usize = 1 << 20;
/// Attempts of one search call before a result is served despite concurrent writes.
const FRESH_ATTEMPTS: u32 = 8;

/// Why a query bypassed the trigram index; the closed set of fallback reasons.
pub(crate) const REASON_SHORT: &str = "literal shorter than 3 bytes";
pub(crate) const REASON_NO_GRAM: &str = "no mandatory trigram";
pub(crate) const REASON_NON_ASCII: &str = "case-insensitive non-ASCII pattern";
pub(crate) const REASON_SINGLE_FILE: &str = "single-file scope";
pub(crate) const REASON_NO_INDEX: &str = "no index yet";

/// A search failure whose display text is the exact model-visible string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SearchError {
    /// A find or grep glob failed to compile.
    #[error("search: invalid glob {pattern}: {reason}")]
    InvalidGlob { pattern: String, reason: String },
    /// The universe walk failed below `path`.
    #[error("search: cannot walk {}: {reason}", path.display())]
    Walk { path: PathBuf, reason: String },
    /// Any other search error, already in its exact text.
    #[error("{0}")]
    Message(Box<str>),
}

impl SearchError {
    /// An error with exact model-visible text.
    pub(crate) fn msg(text: impl Into<Box<str>>) -> Self {
        Self::Message(text.into())
    }
}

/// The search mode named by the `mode` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Find,
    Grep,
    Symbol,
}

/// The workspace directory an indexed search narrows to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexScope<'a> {
    /// The workspace root itself.
    Root,
    /// A directory inside the workspace, relative to the root.
    Directory(&'a Path),
}

impl<'a> IndexScope<'a> {
    /// The directory filter for the index: `None` searches the whole root.
    pub(crate) fn directory(self) -> Option<&'a Path> {
        match self {
            Self::Root => None,
            Self::Directory(rel) => Some(rel),
        }
    }
}

/// Decoded, bounds-checked search arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchArgs {
    pub(crate) mode: Mode,
    pub(crate) pattern: String,
    pub(crate) path: Option<String>,
    pub(crate) glob: Option<String>,
    pub(crate) literal: bool,
    pub(crate) ignore_case: bool,
    pub(crate) limit: usize,
}

/// Decodes the raw argument object strictly: unknown fields and wrong types reject.
pub(crate) fn parse_args(raw: &str) -> Result<SearchArgs, SearchError> {
    let value: sonic_rs::Value = sonic_rs::from_str(raw)
        .map_err(|_| SearchError::msg("search: arguments must be an object"))?;
    let Some(object) = value.as_object() else {
        return Err(SearchError::msg("search: arguments must be an object"));
    };
    let mut mode = None;
    let mut pattern = String::new();
    let mut path = None;
    let mut glob = None;
    let mut literal = false;
    let mut ignore_case = false;
    let mut limit = None;
    for (key, item) in object {
        match key {
            "mode" => mode = Some(string_arg(key, item)?),
            "pattern" => pattern = string_arg(key, item)?,
            "path" => path = Some(string_arg(key, item)?),
            "glob" => glob = Some(string_arg(key, item)?),
            "literal" => literal = bool_arg(key, item)?,
            "ignore_case" => ignore_case = bool_arg(key, item)?,
            "limit" => limit = Some(integer_arg(key, item)?),
            other => {
                return Err(SearchError::msg(format!(
                    "search: unknown argument \"{other}\""
                )));
            }
        }
    }
    let mode = match mode.as_deref() {
        Some("find") => Mode::Find,
        Some("grep") => Mode::Grep,
        Some("symbol") => Mode::Symbol,
        _ => return Err(SearchError::msg(MODE_ERROR)),
    };
    let limit = match limit {
        None => DEFAULT_LIMIT,
        Some(Some(n)) if (1..=MAX_LIMIT).contains(&n) => {
            usize::try_from(n).map_err(|_| SearchError::msg(LIMIT_ERROR))?
        }
        Some(_) => return Err(SearchError::msg(LIMIT_ERROR)),
    };
    Ok(SearchArgs {
        mode,
        pattern,
        path,
        glob,
        literal,
        ignore_case,
        limit,
    })
}

fn string_arg(key: &str, item: &sonic_rs::Value) -> Result<String, SearchError> {
    item.as_str()
        .map(str::to_owned)
        .ok_or_else(|| SearchError::msg(format!("search: {key} must be a string")))
}

fn bool_arg(key: &str, item: &sonic_rs::Value) -> Result<bool, SearchError> {
    item.as_bool()
        .ok_or_else(|| SearchError::msg(format!("search: {key} must be a boolean")))
}

/// An integer argument; `None` inside `Ok` is an integer outside the `u64` range.
fn integer_arg(key: &str, item: &sonic_rs::Value) -> Result<Option<u64>, SearchError> {
    if item.is_u64() {
        Ok(item.as_u64())
    } else if item.is_i64() {
        Ok(None)
    } else {
        Err(SearchError::msg(format!(
            "search: {key} must be an integer"
        )))
    }
}

/// The resolved `path` argument: the directory or file a query runs under.
#[derive(Debug, Clone)]
pub(crate) struct Scope {
    /// The scope as an absolute path.
    pub(crate) abs: PathBuf,
    /// The scope relative to the canonical workspace root, when it lies inside it.
    pub(crate) ws_rel: Option<PathBuf>,
    /// Whether the scope names one regular file.
    pub(crate) is_file: bool,
}

impl Scope {
    /// The whole workspace.
    pub(crate) fn workspace(workspace: &Path) -> Self {
        Self {
            abs: workspace.to_path_buf(),
            ws_rel: Some(PathBuf::new()),
            is_file: false,
        }
    }

    /// The index scope argument: `None` outside the workspace.
    pub(crate) fn index_scope(&self) -> Option<IndexScope<'_>> {
        let rel = self.ws_rel.as_deref()?;
        Some(if rel.as_os_str().is_empty() {
            IndexScope::Root
        } else {
            IndexScope::Directory(rel)
        })
    }

    /// The target for a path relative to this scope directory.
    pub(crate) fn target(&self, relative: &Path) -> Target {
        let abs = self.abs.join(relative);
        let ws_rel = self.ws_rel.as_ref().map(|rel| rel.join(relative));
        let shown = ws_rel
            .as_deref()
            .map_or_else(|| find::display(&abs), find::display);
        Target { abs, ws_rel, shown }
    }

    /// The target for a single-file scope.
    pub(crate) fn self_target(&self) -> Target {
        let shown = self
            .ws_rel
            .as_deref()
            .map_or_else(|| find::display(&self.abs), find::display);
        Target {
            abs: self.abs.clone(),
            ws_rel: self.ws_rel.clone(),
            shown,
        }
    }

    /// The display prefix for rows relative to this scope directory.
    fn prefix(&self) -> String {
        let base = self
            .ws_rel
            .as_deref()
            .map_or_else(|| find::display(&self.abs), find::display);
        if base.is_empty() || base.ends_with('/') {
            base
        } else {
            format!("{base}/")
        }
    }
}

/// Resolves `path` against the workspace root.
pub(crate) async fn resolve_scope(
    workspace: &Path,
    path: Option<&str>,
) -> Result<Scope, SearchError> {
    let Some(raw) = path.filter(|raw| !raw.is_empty()) else {
        return Ok(Scope::workspace(workspace));
    };
    let abs = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        workspace.join(raw)
    };
    let meta = tokio::fs::metadata(&abs)
        .await
        .map_err(|_| SearchError::msg(format!("search: {raw} does not exist")))?;
    let ws_rel = match (
        tokio::fs::canonicalize(workspace).await,
        tokio::fs::canonicalize(&abs).await,
    ) {
        (Ok(root), Ok(full)) => full.strip_prefix(&root).ok().map(Path::to_path_buf),
        _ => None,
    };
    Ok(Scope {
        abs,
        ws_rel,
        is_file: meta.is_file(),
    })
}

/// One file the grep and symbol modes may read.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(crate) abs: PathBuf,
    /// Workspace-relative path; `None` outside the workspace.
    pub(crate) ws_rel: Option<PathBuf>,
    /// The row path: workspace-relative with `/`, or absolute outside the workspace.
    pub(crate) shown: String,
}

/// The searchable files of a scope plus the count of UTF-16 exclusions.
#[derive(Debug, Default)]
pub(crate) struct Universe {
    /// Text files in raw path-byte order.
    pub(crate) files: Vec<Target>,
    /// Files excluded as UTF-16 without a byte order mark.
    pub(crate) utf16: usize,
}

/// Walks the scope once and keeps the text files that the glob admits.
pub(crate) async fn universe(
    scope: &Scope,
    glob: Option<FindGlob>,
) -> Result<Universe, SearchError> {
    let scope = scope.clone();
    tokio::task::spawn_blocking(move || universe_blocking(&scope, glob.as_ref()))
        .await
        .map_err(|error| SearchError::msg(format!("search: {error}")))?
}

fn universe_blocking(scope: &Scope, glob: Option<&FindGlob>) -> Result<Universe, SearchError> {
    let mut out = Universe::default();
    if scope.is_file {
        let name = scope.abs.file_name().map(PathBuf::from).unwrap_or_default();
        if glob.is_some_and(|glob| !glob.is_match(&name)) {
            return Ok(out);
        }
        match find::content_class(&scope.abs) {
            Ok(ContentClass::Text) => out.files.push(scope.self_target()),
            Ok(ContentClass::Utf16NoBom) => out.utf16 = 1,
            Ok(ContentClass::Binary | ContentClass::Oversize) | Err(_) => {}
        }
        return Ok(out);
    }
    let entries = find::walk(&scope.abs)?;
    for Entry {
        path,
        is_dir,
        indexable,
    } in entries
    {
        if is_dir || glob.is_some_and(|glob| !glob.is_match(&path)) {
            continue;
        }
        if indexable {
            out.files.push(scope.target(&path));
        } else if matches!(
            find::content_class(&scope.abs.join(&path)),
            Ok(ContentClass::Utf16NoBom)
        ) {
            out.utf16 += 1;
        }
    }
    out.files
        .sort_by(|a, b| a.shown.as_bytes().cmp(b.shown.as_bytes()));
    Ok(out)
}

/// Per-call host facts the modes need.
pub(crate) struct Call<'a> {
    pub(crate) workspace: &'a Path,
    pub(crate) session: SessionId,
    pub(crate) generation: GenerationId,
    pub(crate) consumer: Consumer,
    pub(crate) turn: Option<TurnId>,
    /// Host services for the reranker; absent only in unit tests.
    pub(crate) services: Option<Arc<dyn Services>>,
}

/// The search engine shared by the tool and its tests.
pub(crate) struct Search {
    pub(crate) symbols: Arc<AtomicBool>,
    pub(crate) index: Arc<Index>,
    pub(crate) seen: Arc<Seen>,
    pub(crate) snapshots: Arc<SnapshotStore>,
    pub(crate) rerank: Option<Arc<dyn Rerank>>,
}

impl Search {
    /// Whether this build and the shared flag both enable symbol mode.
    pub(crate) fn symbols_on(&self) -> bool {
        cfg!(feature = "symbols") && self.symbols.load(Ordering::Relaxed)
    }
    /// Runs one search call from raw arguments to the exact result text.
    ///
    /// A freshness token taken before narrowing is checked after formatting: when
    /// a patch or exec landed meanwhile, the whole query runs again, bounded by
    /// `FRESH_ATTEMPTS`. Past the bound the query fails with the index churn text
    /// instead of serving rows past a completed write; Seen rows and grep
    /// captures are recorded only for the served text, never for a discarded retry.
    pub(crate) async fn execute(
        &self,
        raw: &str,
        call: &Call<'_>,
    ) -> Result<(String, ToolData), SearchError> {
        let args = parse_args(raw)?;
        let mut attempt: u32 = 1;
        loop {
            let token = self.index.freshness(call.workspace);
            let (text, shown, page) = self.execute_once(&args, call).await?;
            if self.index.is_current(call.workspace, &token) {
                for row in shown {
                    self.seen
                        .show(call.session, &row.key, row.digest, row.first, row.last);
                }
                let binding = Binding {
                    session: call.session,
                    generation: call.generation,
                    consumer: call.consumer,
                };
                return Ok((text, page.finish(&self.snapshots, binding)));
            }
            if attempt == FRESH_ATTEMPTS {
                return Err(SearchError::msg(
                    "search: the index kept changing while the query ran",
                ));
            }
            attempt += 1;
        }
    }

    async fn execute_once(
        &self,
        args: &SearchArgs,
        call: &Call<'_>,
    ) -> Result<(String, Vec<SeenRow>, Page), SearchError> {
        match args.mode {
            Mode::Symbol => symbol_mode(self, args, call)
                .await
                .map(|(text, seen, page)| (text, seen, Page::Symbols(page))),
            Mode::Find | Mode::Grep => {
                if args.pattern.is_empty() {
                    return Err(SearchError::msg(EMPTY_PATTERN));
                }
                let scope = resolve_scope(call.workspace, args.path.as_deref()).await?;
                if args.mode == Mode::Find {
                    let (text, page) = self.find(args, &scope, call).await?;
                    Ok((text, Vec::new(), Page::Find(page)))
                } else {
                    let (text, seen, draft) = grep::run(self, args, &scope, call).await?;
                    Ok((text, seen, Page::Grep(draft)))
                }
            }
        }
    }

    async fn find(
        &self,
        args: &SearchArgs,
        scope: &Scope,
        call: &Call<'_>,
    ) -> Result<(String, FindPage), SearchError> {
        let glob = FindGlob::new(&args.pattern)?;
        let wanted = if self.reranks(call) {
            RERANK_WINDOW
        } else {
            args.limit
        };
        let (paths, total) = if scope.is_file {
            let name = scope.abs.file_name().map(PathBuf::from).unwrap_or_default();
            let paths: Vec<(String, bool)> = glob
                .is_match(&name)
                .then(|| (scope.self_target().shown, false))
                .into_iter()
                .collect();
            let total = paths.len();
            (paths, total)
        } else {
            let indexed = match scope.index_scope() {
                Some(index_scope) => self
                    .index
                    .find_entries(call.workspace, index_scope.directory(), &glob, wanted)
                    .await
                    .ok()
                    .flatten(),
                None => None,
            };
            let result = if let Some(result) = indexed {
                result
            } else {
                let root = scope.abs.clone();
                tokio::task::spawn_blocking(move || find::find_with(&root, &glob, wanted))
                    .await
                    .map_err(|error| SearchError::msg(format!("search: {error}")))??
            };
            let prefix = scope.prefix();
            let dirs = result.dirs;
            (
                result
                    .paths
                    .into_iter()
                    .enumerate()
                    .map(|(at, path)| (format!("{prefix}{path}"), at < dirs))
                    .collect(),
                result.total,
            )
        };
        let candidates: Vec<RerankCandidate> = paths
            .iter()
            .map(|(path, _)| RerankCandidate {
                path: path.as_str().into(),
                display: path.as_str().into(),
            })
            .collect();
        let (order, note) = self.rerank(call, &args.pattern, "find", &candidates).await;
        Ok(finish_find(
            paths,
            total,
            args.limit,
            &args.pattern,
            order.as_deref(),
            note.as_deref(),
        ))
    }

    pub(crate) fn reranks(&self, call: &Call<'_>) -> bool {
        self.rerank.is_some() && call.services.is_some()
    }

    /// Calls the reranker on deterministic candidates; returns a validated permutation and its note.
    pub(crate) async fn rerank(
        &self,
        call: &Call<'_>,
        query: &str,
        mode: &str,
        candidates: &[RerankCandidate],
    ) -> (Option<Vec<usize>>, Option<Box<str>>) {
        let (Some(rerank), Some(services)) = (&self.rerank, &call.services) else {
            return (None, None);
        };
        if candidates.is_empty() {
            return (None, None);
        }
        let output = rerank
            .rerank(RerankCall {
                query,
                mode,
                candidates,
                session: call.session,
                turn: call.turn,
                services: Arc::clone(services),
            })
            .await;
        (permutation(&output.order, candidates.len()), output.note)
    }
}

/// Symbol mode with the parser compiled in: validate the pattern, then check the gate.
#[cfg(feature = "symbols")]
async fn symbol_mode(
    search: &Search,
    args: &SearchArgs,
    call: &Call<'_>,
) -> Result<(String, Vec<SeenRow>, SymbolPage), SearchError> {
    let pattern = symbol::Pattern::parse(&args.pattern)?;
    if !search.symbols_on() {
        return Err(SearchError::msg(SYMBOL_OFF));
    }
    symbol::run(search, args, &pattern, call).await
}

/// Symbol mode without the parser: always the off error.
#[cfg(not(feature = "symbols"))]
fn symbol_mode(
    _search: &Search,
    _args: &SearchArgs,
    _call: &Call<'_>,
) -> std::future::Ready<Result<(String, Vec<SeenRow>, SymbolPage), SearchError>> {
    std::future::ready(Err(SearchError::msg(SYMBOL_OFF)))
}

/// Applies a validated rerank order before the `limit` cut, renders, and
/// appends the note last. Each entry is a row path and whether it is a
/// directory.
pub(crate) fn finish_find(
    mut entries: Vec<(String, bool)>,
    total: usize,
    limit: usize,
    pattern: &str,
    order: Option<&[usize]>,
    note: Option<&str>,
) -> (String, FindPage) {
    if let Some(order) = order {
        entries = permute(entries, order);
    }
    entries.truncate(limit);
    let page = FindPage {
        entries: entries
            .iter()
            .map(|(path, dir)| FindEntry {
                path: path.as_str().into(),
                kind: if *dir { "directory" } else { "file" }.into(),
            })
            .collect(),
        truncated: total > entries.len(),
    };
    let dirs = entries.iter().filter(|(_, dir)| *dir).count();
    let paths = entries.into_iter().map(|(path, _)| path).collect();
    let mut text = FindResult { paths, dirs, total }.render(pattern);
    push_line(&mut text, note);
    (text, page)
}

/// Accepts `order` only when it is a permutation of `0..len`.
pub(crate) fn permutation(order: &[u32], len: usize) -> Option<Vec<usize>> {
    if order.len() != len {
        return None;
    }
    let mut taken = vec![false; len];
    let mut out = Vec::with_capacity(len);
    for &index in order {
        let index = usize::try_from(index).ok()?;
        let slot = taken.get_mut(index)?;
        if *slot {
            return None;
        }
        *slot = true;
        out.push(index);
    }
    Some(out)
}

/// Reorders `items` by a validated permutation.
pub(crate) fn permute<T>(items: Vec<T>, order: &[usize]) -> Vec<T> {
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    order
        .iter()
        .filter_map(|&index| slots.get_mut(index).and_then(Option::take))
        .collect()
}

/// Appends one line when present.
pub(crate) fn push_line(text: &mut String, line: Option<&str>) {
    if let Some(line) = line {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
    }
}

/// The canonical absolute path string that keys `Seen`.
pub(crate) fn seen_key(abs: &Path) -> String {
    std::fs::canonicalize(abs)
        .unwrap_or_else(|_| abs.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The registered `search` tool.
struct SearchTool {
    name: Name,
    search: Search,
    spec_off: Arc<ToolSpec>,
    spec_on: Arc<ToolSpec>,
}

/// Builds the `search` tool over the shared flag, index, seen store, and reranker.
pub(crate) fn tool(
    symbols: Arc<AtomicBool>,
    index: Arc<Index>,
    seen: Arc<Seen>,
    snapshots: Arc<SnapshotStore>,
    rerank: Option<Arc<dyn Rerank>>,
) -> Result<Arc<dyn Tool>, RegistrationError> {
    Ok(Arc::new(SearchTool::new(Search {
        symbols,
        index,
        seen,
        snapshots,
        rerank,
    })?))
}

impl SearchTool {
    /// Validates both fixed schemas once, at registration.
    fn new(search: Search) -> Result<Self, RegistrationError> {
        let name = Name::parse("search")?;
        let spec = |description: &str, schema: &str| -> Result<Arc<ToolSpec>, RegistrationError> {
            Ok(Arc::new(ToolSpec {
                name: name.clone(),
                description: description.into(),
                parameters: RawJson::parse(schema)
                    .map_err(|_| RegistrationError::InvalidParameters)?,
                grammar: None,
            }))
        };
        let spec_off = spec(DESCRIPTION_OFF, SCHEMA_OFF)?;
        let spec_on = spec(DESCRIPTION_ON, SCHEMA_ON)?;
        Ok(Self {
            name,
            search,
            spec_off,
            spec_on,
        })
    }

    /// The spec for the flag as it reads now; a flip republishes on the next request.
    fn current_spec(&self) -> Arc<ToolSpec> {
        if self.search.symbols_on() {
            Arc::clone(&self.spec_on)
        } else {
            Arc::clone(&self.spec_off)
        }
    }
}

impl Tool for SearchTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        self.current_spec()
    }

    fn classify(&self, args: &RawJson, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        parse_args(args.as_str())
            .map(|_| ToolClass::Read)
            .map_err(|error| ArgError::message(error.to_string()))
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let host = Call {
                workspace: cx.workspace().as_path(),
                session: cx.session(),
                generation: cx.generation(),
                consumer: cx.consumer(),
                turn: cx.turn(),
                services: Some(cx.services()),
            };
            match self.search.execute(call.args.as_str(), &host).await {
                Ok((text, data)) => {
                    let mut output = ToolOutput::from_text(text);
                    output.data = Some(data);
                    ToolOutcome::Ok(Box::new(output))
                }
                Err(error) => ToolOutcome::Err(ToolError::message(error.to_string())),
            }
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A search engine over a fresh seen store, with no reranker.
    pub(crate) fn engine(symbols: bool, index_root: Option<PathBuf>) -> Search {
        Search {
            symbols: Arc::new(AtomicBool::new(symbols)),
            index: Index::new(index_root),
            seen: Seen::new(),
            snapshots: Arc::new(SnapshotStore::new([5; 16])),
            rerank: None,
        }
    }

    pub(crate) fn host(workspace: &Path) -> Call<'_> {
        Call {
            workspace,
            session: SessionId::new_v7(),
            generation: GenerationId::new(std::num::NonZeroU64::MIN),
            consumer: Consumer::Model,
            turn: None,
            services: None,
        }
    }

    pub(crate) async fn run(
        search: &Search,
        workspace: &Path,
        raw: &str,
    ) -> Result<String, String> {
        search
            .execute(raw, &host(workspace))
            .await
            .map(|(text, _)| text)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn write(root: &Path, relative: &str, bytes: &[u8]) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[tokio::test]
    async fn search_error_paths() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", b"alpha\n");
        let search = engine(false, None);
        let cases = [
            (
                r#"{"mode":"grep","pattern":""}"#,
                "search: pattern must not be empty",
            ),
            (
                r#"{"mode":"findall","pattern":"x"}"#,
                "search: mode must be \"find\", \"grep\", or \"symbol\"",
            ),
            (
                r#"{"mode":"grep","pattern":"a("}"#,
                "search: invalid regular expression: a(: unclosed group",
            ),
            (
                r#"{"mode":"find","pattern":"a[","limit":5}"#,
                "search: invalid glob a[: unclosed character class; missing ']'",
            ),
            (
                r#"{"mode":"grep","pattern":"x","limit":0}"#,
                "search: limit must be between 1 and 1000",
            ),
            (r#"{"mode":"symbol","pattern":"alpha"}"#, SYMBOL_OFF),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                run(&search, dir.path(), raw).await,
                Err(expected.to_owned()),
                "{raw}"
            );
        }
    }

    #[test]
    fn argument_errors_are_exact() {
        let cases = [
            (
                r#"{"mode":"grep","pattern":"x","context":2}"#,
                "search: unknown argument \"context\"",
            ),
            (
                r#"{"mode":"grep","pattern":7}"#,
                "search: pattern must be a string",
            ),
            (
                r#"{"mode":"grep","pattern":"x","literal":"yes"}"#,
                "search: literal must be a boolean",
            ),
            (
                r#"{"mode":"grep","pattern":"x","limit":2.5}"#,
                "search: limit must be an integer",
            ),
            (r#"{"mode":"grep","pattern":"x","limit":-1}"#, LIMIT_ERROR),
            (r#"{"mode":"grep","pattern":"x","limit":1001}"#, LIMIT_ERROR),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                parse_args(raw).map_err(|error| error.to_string()),
                Err(expected.to_owned()),
                "{raw}"
            );
        }
        let args = parse_args(r#"{"mode":"find","pattern":"*.rs"}"#).unwrap();
        assert_eq!(
            (args.limit, args.literal, args.ignore_case),
            (DEFAULT_LIMIT, false, false)
        );
    }

    #[test]
    fn schemas_differ_only_by_the_symbol_additions() {
        assert!(RawJson::parse(SCHEMA_OFF).is_ok() && RawJson::parse(SCHEMA_ON).is_ok());
        let on = SCHEMA_ON
            .replace(r#","symbol"]"#, "]")
            .replace(" symbol: a name or an A::b path, [2] for the second of equal names, or * with path set to one file.", "");
        assert_eq!(on, SCHEMA_OFF);
        assert!(DESCRIPTION_ON.starts_with(DESCRIPTION_OFF));
    }

    #[test]
    fn find_reranks_before_the_limit_and_notes_last() {
        let paths = || {
            ["a.rs", "b.rs", "c.rs"]
                .map(|path| (path.to_owned(), false))
                .to_vec()
        };
        let order = permutation(&[2, 1, 0], 3);
        assert_eq!(
            finish_find(paths(), 3, 2, "*.rs", order.as_deref(), Some("[reranked]")).0,
            "c.rs\nb.rs\n[Truncated: 1 more paths]\n[reranked]"
        );
        let invalid = permutation(&[0, 0, 1], 3);
        assert_eq!(
            finish_find(
                paths(),
                3,
                2,
                "*.rs",
                invalid.as_deref(),
                Some("[reranked]")
            )
            .0,
            "a.rs\nb.rs\n[Truncated: 1 more paths]\n[reranked]"
        );
    }

    #[test]
    fn rerank_order_must_be_a_permutation() {
        assert_eq!(permutation(&[2, 0, 1], 3), Some(vec![2, 0, 1]));
        assert_eq!(permutation(&[0, 0, 1], 3), None);
        assert_eq!(permutation(&[0, 1], 3), None);
        assert_eq!(permutation(&[0, 3, 1], 3), None);
        assert_eq!(
            permute(vec!["a", "b", "c"], &[2, 0, 1]),
            vec!["c", "a", "b"]
        );
    }
}
