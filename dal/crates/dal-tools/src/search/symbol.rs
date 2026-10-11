//! Symbol mode: definitions from a fresh parse of the current bytes.
//!
//! The pattern is a `::`-separated suffix of a qualified name. A segment may
//! carry `[k]`, the 1-based source-order ordinal among equal names in one file.
//! A leading `::` pins the match to the full qualified name. The index only
//! narrows candidate files; every row comes from a parse of bytes read now.

use std::path::Path;

use dal_core::{SymbolHit, SymbolPage};

use super::find;
use super::grep::SeenRow;
use super::{
    Call, REASON_NO_INDEX, REASON_SHORT, Scope, Search, SearchArgs, SearchError, Target, permute,
    push_line, resolve_scope, universe,
};
use crate::parse::{self, Def, ParseFailure};
use crate::{RerankCandidate, digest32, tag8};

/// At most this many candidate files are parsed per query.
const MAX_PARSED: usize = 500;
/// A single match reveals whole only within the read limits.
const REVEAL_LINES: u32 = 2000;
const REVEAL_BYTES: usize = 50 * 1024;
const NEEDS_FILE: &str = "search: pattern * needs path set to one file.";

/// A validated symbol pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pattern {
    text: String,
    /// `*`: outline one file.
    star: bool,
    /// A leading `::` matches the full qualified name only.
    absolute: bool,
    segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment {
    name: String,
    ordinal: Option<u32>,
}

impl Pattern {
    /// Validates the pattern grammar; failures carry the exact reason.
    pub(crate) fn parse(text: &str) -> Result<Self, SearchError> {
        let invalid = |reason: &str| {
            SearchError::msg(format!("search: invalid symbol pattern {text}: {reason}."))
        };
        if text.is_empty() {
            return Err(invalid("empty"));
        }
        if text == "*" {
            return Ok(Self {
                text: text.to_owned(),
                star: true,
                absolute: false,
                segments: Vec::new(),
            });
        }
        if text.contains('*') {
            return Err(invalid("* with more than one path"));
        }
        let (absolute, body) = match text.strip_prefix("::") {
            Some(body) => (true, body),
            None => (false, text),
        };
        let segments = body
            .split("::")
            .map(Segment::parse)
            .collect::<Result<Vec<_>, _>>()
            .map_err(invalid)?;
        Ok(Self {
            text: text.to_owned(),
            star: false,
            absolute,
            segments,
        })
    }

    /// The last segment's name: the literal the index narrows by.
    fn last_name(&self) -> &str {
        self.segments
            .last()
            .map_or("", |segment| segment.name.as_str())
    }

    /// Whether `def`, one of the file's `defs`, matches segment-wise from the tail.
    fn matches(&self, def: &Def, defs: &[Def]) -> bool {
        let parts: Vec<&str> = def.qualified.split("::").collect();
        let wanted = self.segments.len();
        if parts.len() < wanted || (self.absolute && parts.len() != wanted) {
            return false;
        }
        let offset = parts.len() - wanted;
        self.segments.iter().enumerate().all(|(at, segment)| {
            let depth = offset + at;
            parts[depth] == segment.name
                && segment
                    .ordinal
                    .is_none_or(|ordinal| ordinal_at(def, defs, &parts, depth) == ordinal)
        })
    }
}

impl Segment {
    fn parse(part: &str) -> Result<Self, &'static str> {
        let (name, ordinal) = match part.split_once('[') {
            Some((name, rest)) => (name, Some(rest)),
            None => (part, None),
        };
        if name.is_empty() {
            return Err("empty segment");
        }
        let Some(rest) = ordinal else {
            return Ok(Self {
                name: name.to_owned(),
                ordinal: None,
            });
        };
        let digits = rest
            .strip_suffix(']')
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or("ordinal must be a number")?;
        let value = digits.parse::<u32>().unwrap_or(u32::MAX);
        if value == 0 {
            return Err("ordinal must be at least 1");
        }
        Ok(Self {
            name: name.to_owned(),
            ordinal: Some(value),
        })
    }
}

/// The ordinal of the qualified-name segment at `depth`: the definition's own
/// ordinal for its last segment, else the ordinal of the enclosing definition
/// named by that prefix (1 when no definition carries the prefix).
fn ordinal_at(def: &Def, defs: &[Def], parts: &[&str], depth: usize) -> u32 {
    if depth + 1 == parts.len() {
        return def.ordinal;
    }
    let prefix = parts[..=depth].join("::");
    defs.iter()
        .filter(|outer| {
            !std::ptr::eq(*outer, def)
                && *outer.qualified == *prefix
                && outer.byte_start <= def.byte_start
                && def.byte_end <= outer.byte_end
        })
        .max_by_key(|outer| outer.byte_start)
        .map_or(1, |outer| outer.ordinal)
}

/// The printed name: the qualified name, plus `[k]` from the second equal name on.
fn label(def: &Def) -> String {
    if def.ordinal > 1 {
        format!("{}[{}]", def.qualified, def.ordinal)
    } else {
        def.qualified.to_string()
    }
}

/// `<path>:<first>-<last> <kind> <name>`.
fn row(path: &str, def: &Def) -> String {
    format!(
        "{path}:{}-{} {} {}",
        def.first,
        def.last,
        def.kind.as_str(),
        label(def)
    )
}

/// The extension without its dot, or `unknown`.
fn lang_name(path: &Path) -> String {
    path.extension().map_or_else(
        || "unknown".to_owned(),
        |ext| ext.to_string_lossy().into_owned(),
    )
}

/// Text files of at most 16 MiB are parsed; binary and UTF-16 files are skipped.
fn parsable(bytes: &[u8]) -> bool {
    u64::try_from(bytes.len()).is_ok_and(|len| len <= find::MAX_INDEXABLE_BYTES)
        && find::classify_probe(bytes).indexable()
}

async fn seen_key(abs: &Path) -> String {
    tokio::fs::canonicalize(abs)
        .await
        .unwrap_or_else(|_| abs.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Runs symbol mode after the pattern and the gate have passed. The reveal row
/// is returned, not recorded: the caller records it only for the served result.
#[expect(
    clippy::too_many_lines,
    reason = "one symbol lookup walks pattern, gate, and reveal in place"
)]
pub(crate) async fn run(
    search: &Search,
    args: &SearchArgs,
    pattern: &Pattern,
    call: &Call<'_>,
) -> Result<(String, Vec<SeenRow>, SymbolPage), SearchError> {
    if pattern.star {
        let (text, page) = outline(args, call).await?;
        return Ok((text, Vec::new(), page));
    }
    let scope = resolve_scope(call.workspace, args.path.as_deref()).await?;
    let (targets, narrowing) =
        candidates(search, &scope, pattern.last_name(), call.workspace).await?;
    let Funnel {
        single_bytes,
        files,
        mut found,
        cut,
        queue_full,
        failures,
    } = funnel(targets, pattern).await;
    let complete = !cut && !queue_full && failures.is_empty();
    let mut seen_rows = Vec::new();
    let mut rerank_note = None;
    let mut text = if found.is_empty() {
        let scope_name = args
            .path
            .as_deref()
            .filter(|path| !path.is_empty())
            .unwrap_or("the workspace");
        format!("No definitions match {} in {scope_name}.", pattern.text)
    } else if let ([single], Some(bytes)) = (found.as_slice(), single_bytes.as_deref())
        && complete
        && let Some((body, first, last)) = reveal(&files[single.file], bytes, &single.def)
    {
        let target = &files[single.file];
        seen_rows.push(SeenRow {
            key: seen_key(&target.abs).await,
            digest: digest32(bytes),
            first,
            last,
        });
        body
    } else {
        let candidates: Vec<RerankCandidate> = found
            .iter()
            .map(|hit| {
                let shown = files[hit.file].shown.as_str();
                RerankCandidate {
                    path: shown.into(),
                    display: row(shown, &hit.def).into(),
                }
            })
            .collect();
        let (order, note) = search
            .rerank(call, &pattern.text, "symbol", &candidates)
            .await;
        rerank_note = note;
        if let Some(order) = order {
            found = permute(found, &order);
        }
        let mut text: String = found
            .iter()
            .map(|hit| row(&files[hit.file].shown, &hit.def))
            .collect::<Vec<_>>()
            .join("\n");
        let footer = if found.len() == 1 && complete {
            format!(
                "[1 definition is over {REVEAL_LINES} lines or 50 KiB. Read its lines to see it whole.]"
            )
        } else {
            format!(
                "[{} definitions. Search a longer name to see one whole with its tag.]",
                found.len()
            )
        };
        push_line(&mut text, Some(&footer));
        text
    };
    if cut {
        push_line(
            &mut text,
            Some(&format!(
                "[{} definitions shown; parse budget reached.]",
                found.len()
            )),
        );
    }
    if queue_full {
        push_line(&mut text, Some("[Parse queue full; retry.]"));
    }
    for failure in &failures {
        push_line(&mut text, Some(failure));
    }
    if let Some(reason) = narrowing {
        push_line(&mut text, Some(&format!("[No index narrowing: {reason}.]")));
    }
    push_line(&mut text, rerank_note.as_deref());
    let page = SymbolPage {
        hits: found
            .iter()
            .map(|hit| symbol_hit(&files[hit.file].shown, &hit.def))
            .collect(),
        truncated: !complete,
    };
    Ok((text, seen_rows, page))
}

/// The typed row of one definition.
fn symbol_hit(path: &str, def: &Def) -> SymbolHit {
    SymbolHit {
        path: path.into(),
        first: u64::from(def.first),
        last: u64::from(def.last),
        kind: def.kind.as_str().into(),
        name: label(def).into(),
    }
}

/// Candidate files in path byte order and the reason when the index did not narrow them.
async fn candidates(
    search: &Search,
    scope: &Scope,
    name: &str,
    workspace: &Path,
) -> Result<(Vec<Target>, Option<&'static str>), SearchError> {
    if scope.is_file {
        return Ok((vec![scope.self_target()], None));
    }
    let narrowed = match scope.index_scope() {
        _ if name.len() < 3 => Err(REASON_SHORT),
        None => Err(REASON_NO_INDEX),
        Some(index_scope) => {
            let clauses = [vec![name.as_bytes().to_vec()]];
            match search
                .index
                .search_candidates(workspace, &clauses, false, index_scope)
                .await
            {
                Ok(Some(paths)) => Ok(paths),
                Ok(None) | Err(_) => Err(REASON_NO_INDEX),
            }
        }
    };
    let (mut targets, reason) = match narrowed {
        Ok(paths) => {
            let targets = paths
                .into_iter()
                .map(|relative| Target {
                    abs: workspace.join(&relative),
                    shown: find::display(&relative),
                    ws_rel: Some(relative),
                })
                .collect();
            (targets, None)
        }
        Err(reason) => (universe(scope, None).await?.files, Some(reason)),
    };
    targets.retain(|target| parse::language(&target.abs).is_some());
    targets.sort_by(|a, b| a.shown.as_bytes().cmp(b.shown.as_bytes()));
    Ok((targets, reason))
}

/// One matching definition of one parsed file.
struct Hit {
    file: usize,
    def: Def,
}

/// The parsed files that matched, their hits in path and source order, and the cut notes.
#[derive(Default)]
struct Funnel {
    /// The bytes of the only matching file while exactly one definition matches.
    single_bytes: Option<Vec<u8>>,
    files: Vec<Target>,
    found: Vec<Hit>,
    cut: bool,
    queue_full: bool,
    failures: Vec<String>,
}

/// Reads a candidate when it is a text file of at most 16 MiB; the size is checked first.
async fn read_parsable(abs: &Path) -> Option<Vec<u8>> {
    let meta = tokio::fs::metadata(abs).await.ok()?;
    if meta.len() > find::MAX_INDEXABLE_BYTES {
        return None;
    }
    let bytes = tokio::fs::read(abs).await.ok()?;
    parsable(&bytes).then_some(bytes)
}

async fn funnel(targets: Vec<Target>, pattern: &Pattern) -> Funnel {
    let mut out = Funnel::default();
    let mut parsed = 0;
    for target in targets {
        if parsed == MAX_PARSED {
            out.cut = true;
            break;
        }
        let Some(bytes) = read_parsable(&target.abs).await else {
            continue;
        };
        parsed += 1;
        let defs = match parse::definitions(&target.abs, &bytes).await {
            Ok(defs) => defs,
            Err(ParseFailure::Budget) => {
                out.cut = true;
                continue;
            }
            Err(ParseFailure::TooManyParses) => {
                out.queue_full = true;
                continue;
            }
            Err(failure @ ParseFailure::Panic) => {
                out.failures.push(format!(
                    "search: parser failed on {}: {failure}.",
                    target.shown
                ));
                continue;
            }
            Err(ParseFailure::Unsupported) => continue,
        };
        let matched: Vec<Def> = defs
            .iter()
            .filter(|def| pattern.matches(def, &defs))
            .cloned()
            .collect();
        if matched.is_empty() {
            continue;
        }
        let file = out.files.len();
        out.found
            .extend(matched.into_iter().map(|def| Hit { file, def }));
        out.files.push(target);
        out.single_bytes = (out.found.len() == 1).then_some(bytes);
    }
    out
}

/// The whole-definition form when it fits the read limits: header, rows, and the shown interval.
fn reveal(target: &Target, bytes: &[u8], def: &Def) -> Option<(String, u64, u64)> {
    if def.last < def.first || def.last - def.first + 1 > REVEAL_LINES {
        return None;
    }
    let span = bytes.get(def.byte_start..def.byte_end)?;
    let first = usize::try_from(def.first).ok()?.checked_sub(1)?;
    let count = usize::try_from(def.last - def.first + 1).ok()?;
    let lines: Vec<&[u8]> = bytes
        .split(|&byte| byte == b'\n')
        .skip(first)
        .take(count)
        .collect();
    let size: usize = lines.iter().map(|line| line.len() + 1).sum();
    if lines.len() != count || size > REVEAL_BYTES {
        return None;
    }
    let mut text = format!("{} tag {}", row(&target.shown, def), tag8("def", span));
    for (number, line) in (def.first..).zip(lines) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!("\n{number}:{}", String::from_utf8_lossy(line)),
        );
    }
    Some((text, u64::from(def.first), u64::from(def.last)))
}

/// `pattern = "*"`: every definition of one file in source order.
async fn outline(args: &SearchArgs, call: &Call<'_>) -> Result<(String, SymbolPage), SearchError> {
    let Some(raw) = args.path.as_deref().filter(|path| !path.is_empty()) else {
        return Err(SearchError::msg(NEEDS_FILE));
    };
    let scope = resolve_scope(call.workspace, Some(raw)).await?;
    if !scope.is_file {
        let is_dir = tokio::fs::metadata(&scope.abs)
            .await
            .is_ok_and(|meta| meta.is_dir());
        return Err(SearchError::msg(if is_dir {
            NEEDS_FILE.to_owned()
        } else {
            format!("search: {raw} is not a file.")
        }));
    }
    let no_support = || {
        SearchError::msg(format!(
            "search: {raw} has no symbol support ({}).",
            lang_name(&scope.abs)
        ))
    };
    if parse::language(&scope.abs).is_none() {
        return Err(no_support());
    }
    let oversized = tokio::fs::metadata(&scope.abs)
        .await
        .is_ok_and(|meta| meta.len() > find::MAX_INDEXABLE_BYTES);
    if oversized {
        return Err(SearchError::msg(format!(
            "search: {raw} is larger than 16 MiB; symbol outline is skipped."
        )));
    }
    let bytes = tokio::fs::read(&scope.abs)
        .await
        .map_err(|_| SearchError::msg(format!("search: {raw} does not exist")))?;
    if !parsable(&bytes) {
        return Err(no_support());
    }
    let shown = scope.self_target().shown;
    match parse::definitions(&scope.abs, &bytes).await {
        Ok(defs) => {
            let mut text = defs
                .iter()
                .map(|def| row(&shown, def))
                .collect::<Vec<_>>()
                .join("\n");
            push_line(&mut text, Some(&format!("[{} definitions.]", defs.len())));
            let page = SymbolPage {
                hits: defs.iter().map(|def| symbol_hit(&shown, def)).collect(),
                truncated: false,
            };
            Ok((text, page))
        }
        Err(ParseFailure::Budget) => Ok((
            "[0 definitions shown; parse budget reached.]".to_owned(),
            cut_page(),
        )),
        Err(ParseFailure::TooManyParses) => {
            Ok(("[Parse queue full; retry.]".to_owned(), cut_page()))
        }
        Err(failure @ ParseFailure::Panic) => Err(SearchError::msg(format!(
            "search: parser failed on {raw}: {failure}."
        ))),
        Err(ParseFailure::Unsupported) => Err(no_support()),
    }
}

/// An empty page for an outline the parser could not finish.
fn cut_page() -> SymbolPage {
    SymbolPage {
        hits: Box::new([]),
        truncated: true,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use proptest::prelude::*;

    use super::super::tests::{engine, run as search, write};
    use super::super::{SYMBOL_OFF, SearchTool};
    use super::*;
    use crate::parse::hooks;

    const TWO_BLOCKS: &str =
        "mod engine {\n    pub fn process_block() {}\n}\nfn process_block() {}\n";

    fn query(pattern: &str) -> String {
        format!(
            r#"{{"mode":"symbol","pattern":{}}}"#,
            sonic_rs::to_string(pattern).unwrap()
        )
    }

    fn query_in(pattern: &str, path: &str) -> String {
        format!(
            r#"{{"mode":"symbol","pattern":{},"path":{}}}"#,
            sonic_rs::to_string(pattern).unwrap(),
            sonic_rs::to_string(path).unwrap()
        )
    }

    /// An independent def tag: BLAKE3 over `def:` and the raw bytes, first 8 uppercase hex.
    fn oracle_tag(span: &[u8]) -> String {
        let digest = blake3::hash(&[b"def:".as_slice(), span].concat());
        digest.as_bytes()[..4]
            .iter()
            .fold(String::new(), |mut out, byte| {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{byte:02X}"));
                out
            })
    }

    #[test]
    fn pattern_errors_are_exact_and_ordered() {
        let cases = [
            ("", "empty"),
            ("a::", "empty segment"),
            ("::", "empty segment"),
            ("[2]", "empty segment"),
            ("a[x]", "ordinal must be a number"),
            ("a[2", "ordinal must be a number"),
            ("a[0]", "ordinal must be at least 1"),
            ("A::*", "* with more than one path"),
        ];
        for (text, reason) in cases {
            assert_eq!(
                Pattern::parse(text).map_err(|error| error.to_string()),
                Err(format!("search: invalid symbol pattern {text}: {reason}.")),
            );
        }
        let pattern = Pattern::parse("::Engine::run[2]").unwrap();
        assert!(pattern.absolute);
        assert_eq!(pattern.last_name(), "run");
        assert_eq!(pattern.segments[1].ordinal, Some(2));
    }

    #[tokio::test]
    async fn symbol_suffix_matching() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", TWO_BLOCKS.as_bytes());
        let engine = engine(true, None);
        let both = search(&engine, dir.path(), &query("process_block"))
            .await
            .unwrap();
        assert_eq!(
            both,
            "a.rs:2-2 function engine::process_block\na.rs:4-4 function process_block[2]\n\
             [2 definitions. Search a longer name to see one whole with its tag.]\n\
             [No index narrowing: no index yet.]"
        );
        let nested = search(&engine, dir.path(), &query("engine::process_block"))
            .await
            .unwrap();
        let span = b"pub fn process_block() {}";
        assert!(
            nested.starts_with(&format!(
                "a.rs:2-2 function engine::process_block tag {}\n2:    pub fn process_block() {{}}",
                oracle_tag(span)
            )),
            "{nested}"
        );
        let top = search(&engine, dir.path(), &query("::process_block"))
            .await
            .unwrap();
        assert!(
            top.starts_with("a.rs:4-4 function process_block[2] tag "),
            "{top}"
        );
        let second = search(&engine, dir.path(), &query("process_block[2]"))
            .await
            .unwrap();
        assert_eq!(second.lines().next(), top.lines().next());
        let none = search(&engine, dir.path(), &query("process_block[3]"))
            .await
            .unwrap();
        assert!(
            none.starts_with("No definitions match process_block[3] in the workspace."),
            "{none}"
        );
        let regex_like = search(&engine, dir.path(), &query("block[2]"))
            .await
            .unwrap();
        assert!(
            regex_like.starts_with("No definitions match block[2] in the workspace."),
            "{regex_like}"
        );
    }

    #[tokio::test]
    async fn symbol_outline_via_star() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", TWO_BLOCKS.as_bytes());
        let engine = engine(true, None);
        let outline = search(&engine, dir.path(), &query_in("*", "a.rs"))
            .await
            .unwrap();
        assert_eq!(
            outline,
            "a.rs:1-3 module engine\na.rs:2-2 function engine::process_block\n\
             a.rs:4-4 function process_block[2]\n[3 definitions.]"
        );
        for raw in [query("*"), query_in("*", ".")] {
            assert_eq!(
                search(&engine, dir.path(), &raw).await,
                Err(NEEDS_FILE.to_owned())
            );
        }
        assert_eq!(
            search(&engine, dir.path(), &query_in("*", "gone.rs")).await,
            Err("search: gone.rs does not exist".to_owned())
        );
        write(dir.path(), "notes.txt", b"text\n");
        assert_eq!(
            search(&engine, dir.path(), &query_in("*", "notes.txt")).await,
            Err("search: notes.txt has no symbol support (txt).".to_owned())
        );
    }

    #[tokio::test]
    async fn symbol_reveal_and_tag() {
        let dir = tempfile::tempdir().unwrap();
        let source = "// header\nfn apply(x: u32) -> u32 {\n    x + 1\n}\n";
        write(dir.path(), "lib.rs", source.as_bytes());
        let text = search(&engine(true, None), dir.path(), &query("apply"))
            .await
            .unwrap();
        let span = b"fn apply(x: u32) -> u32 {\n    x + 1\n}";
        assert_eq!(
            text,
            format!(
                "lib.rs:2-4 function apply tag {}\n2:fn apply(x: u32) -> u32 {{\n3:    x + 1\n4:}}\n\
                 [No index narrowing: no index yet.]",
                oracle_tag(span)
            )
        );
    }

    fn long_fn(name: &str, body_lines: usize) -> String {
        format!(
            "fn {name}() {{\n{}}}\n",
            "    let _ = 0;\n".repeat(body_lines)
        )
    }

    #[tokio::test]
    async fn symbol_reveal_limits() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "big.rs", long_fn("huge_fn", 2998).as_bytes());
        write(dir.path(), "fit.rs", long_fn("fits_fn", 1997).as_bytes());
        let engine = engine(true, None);
        let huge = search(&engine, dir.path(), &query("huge_fn"))
            .await
            .unwrap();
        assert_eq!(
            huge.lines().take(2).collect::<Vec<_>>(),
            [
                "big.rs:1-3000 function huge_fn",
                "[1 definition is over 2000 lines or 50 KiB. Read its lines to see it whole.]"
            ]
        );
        let fits = search(&engine, dir.path(), &query("fits_fn"))
            .await
            .unwrap();
        assert!(
            fits.starts_with("fit.rs:1-1999 function fits_fn tag "),
            "{fits}"
        );
        assert!(fits.contains("\n1999:}"), "{fits}");
    }

    #[tokio::test]
    async fn symbol_flag_off() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", TWO_BLOCKS.as_bytes());
        let engine = engine(false, None);
        assert_eq!(
            search(&engine, dir.path(), &query("process_block")).await,
            Err(SYMBOL_OFF.to_owned())
        );
        assert_eq!(
            search(&engine, dir.path(), &query_in("*", "a.rs")).await,
            Err(SYMBOL_OFF.to_owned())
        );
        let tool = SearchTool::new(engine).unwrap();
        assert!(!tool.current_spec().parameters.as_str().contains("symbol"));
        assert!(!tool.current_spec().description.contains("mode \"symbol\""));
    }

    #[test]
    fn symbol_flag_on_schema_swap() {
        let tool = SearchTool::new(engine(false, None)).unwrap();
        let off = tool.current_spec();
        tool.search
            .symbols
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let on = tool.current_spec();
        assert!(
            on.parameters
                .as_str()
                .contains(r#""enum":["find","grep","symbol"]"#)
        );
        assert!(
            on.description
                .ends_with("pattern * with path set to one file lists that file's definitions.")
        );
        tool.search
            .symbols
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(tool.current_spec(), off);
    }

    #[tokio::test]
    async fn symbol_parse_budget_skip() {
        let dir = tempfile::tempdir().unwrap();
        let mut source = String::from("int target_fn(void) { return 0; }\n");
        let mut index = 0;
        while source.len() < 4 << 20 {
            let _ = std::fmt::Write::write_fmt(
                &mut source,
                format_args!("int filler_{index}(int a, int b) {{ return a * b + {index}; }}\n"),
            );
            index += 1;
        }
        write(dir.path(), "big.c", source.as_bytes());
        let path = dir.path().join("big.c");
        hooks::set_budget(&path, Duration::from_millis(1));
        let text = search(&engine(true, None), dir.path(), &query("target_fn"))
            .await
            .unwrap();
        hooks::reset(&path);
        assert_eq!(
            text,
            "No definitions match target_fn in the workspace.\n\
             [0 definitions shown; parse budget reached.]\n[No index narrowing: no index yet.]"
        );
    }

    #[tokio::test]
    async fn symbol_panic_fault_injection() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", b"fn shared_name() {}\n");
        write(dir.path(), "b.rs", b"fn shared_name() {}\n");
        let panicking = dir.path().join("a.rs");
        hooks::set_panic(&panicking);
        let text = search(&engine(true, None), dir.path(), &query("shared_name"))
            .await
            .unwrap();
        let outline = search(&engine(true, None), dir.path(), &query_in("*", "a.rs")).await;
        hooks::reset(&panicking);
        assert!(
            text.starts_with(
                "b.rs:1-1 function shared_name\n[1 definitions. Search a longer name to see one whole with its tag.]"
            ),
            "{text}"
        );
        assert!(
            text.contains("\nsearch: parser failed on a.rs: the parser panicked."),
            "{text}"
        );
        assert_eq!(
            outline,
            Err("search: parser failed on a.rs: the parser panicked.".to_owned())
        );
        let after = search(&engine(true, None), dir.path(), &query_in("*", "a.rs"))
            .await
            .unwrap();
        assert_eq!(after, "a.rs:1-1 function shared_name\n[1 definitions.]");
    }

    #[tokio::test]
    async fn symbol_cache_correctness() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "c.rs", b"fn cached_one() {}\n");
        let path = dir.path().join("c.rs");
        hooks::forget(&path);
        let engine = engine(true, None);
        let before = hooks::parse_count(&path);
        let first = search(&engine, dir.path(), &query_in("*", "c.rs"))
            .await
            .unwrap();
        assert_eq!(first, "c.rs:1-1 function cached_one\n[1 definitions.]");
        assert_eq!(hooks::parse_count(&path), before + 1);
        search(&engine, dir.path(), &query_in("*", "c.rs"))
            .await
            .unwrap();
        assert_eq!(hooks::parse_count(&path), before + 1);
        write(dir.path(), "c.rs", b"fn cached_two() {}\n");
        let second = search(&engine, dir.path(), &query_in("*", "c.rs"))
            .await
            .unwrap();
        assert_eq!(second, "c.rs:1-1 function cached_two\n[1 definitions.]");
        assert_eq!(hooks::parse_count(&path), before + 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn symbol_cpu_admission() {
        let dir = tempfile::tempdir().unwrap();
        for file in 0..64 {
            write(
                dir.path(),
                &format!("f{file}.rs"),
                long_fn(&format!("admitted_{file}"), 200).as_bytes(),
            );
        }
        let engine = Arc::new(engine(true, None));
        let root: Arc<Path> = dir.path().into();
        let mut tasks = tokio::task::JoinSet::new();
        for file in 0..64 {
            let engine = Arc::clone(&engine);
            let root = Arc::clone(&root);
            tasks.spawn(async move {
                search(&engine, &root, &query_in("*", &format!("f{file}.rs"))).await
            });
        }
        while let Some(joined) = tasks.join_next().await {
            let text = joined.unwrap().unwrap();
            assert!(
                text.ends_with("[1 definitions.]") || text == "[Parse queue full; retry.]",
                "{text}"
            );
        }
        assert!(hooks::peak_in_flight() <= crate::parse::MAX_PARSSES_IN_FLIGHT);
    }

    #[tokio::test]
    async fn symbol_determinism() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "b.rs", b"fn run() {}\nmod m { fn run() {} }\n");
        write(dir.path(), "a.rs", b"fn run() {}\n");
        let engine = engine(true, None);
        let first = search(&engine, dir.path(), &query("run")).await.unwrap();
        let second = search(&engine, dir.path(), &query("run")).await.unwrap();
        assert_eq!(first, second);
        let rows: Vec<&str> = first.lines().take(3).collect();
        assert_eq!(
            rows,
            [
                "a.rs:1-1 function run",
                "b.rs:1-1 function run",
                "b.rs:2-2 function m::run[2]"
            ]
        );
    }

    /// One generated line: a free function, or a one-line module holding one function.
    type Item = (bool, usize);

    const NAMES: [&str; 2] = ["alpha_fn", "beta_fn"];

    /// Reference definitions of one generated file: (line, kind, qualified, ordinal).
    fn reference_defs(items: &[Item]) -> Vec<(usize, &'static str, String, u32)> {
        let mut counts = std::collections::HashMap::new();
        let mut ordinal = |name: &str| {
            let count = counts.entry(name.to_owned()).or_insert(0_u32);
            *count += 1;
            *count
        };
        let mut defs = Vec::new();
        for (at, &(in_mod, name)) in items.iter().enumerate() {
            let line = at + 1;
            if in_mod {
                defs.push((line, "module", "m".to_owned(), ordinal("m")));
                defs.push((
                    line,
                    "function",
                    format!("m::{}", NAMES[name]),
                    ordinal(NAMES[name]),
                ));
            } else {
                defs.push((
                    line,
                    "function",
                    NAMES[name].to_owned(),
                    ordinal(NAMES[name]),
                ));
            }
        }
        defs
    }

    /// Reference rows for a pattern: segment-wise tail match, then path bytes and source order.
    fn reference_rows(files: &[Vec<Item>], absolute: bool, segments: &[&str]) -> Vec<String> {
        let mut rows = Vec::new();
        for (file, items) in files.iter().enumerate() {
            for (line, kind, qualified, ordinal) in reference_defs(items) {
                let parts: Vec<&str> = qualified.split("::").collect();
                let fits = if absolute {
                    parts.len() == segments.len()
                } else {
                    parts.len() >= segments.len()
                };
                if fits && parts[parts.len() - segments.len()..] == *segments {
                    let label = if ordinal > 1 {
                        format!("{qualified}[{ordinal}]")
                    } else {
                        qualified
                    };
                    rows.push(format!("f{file}.rs:{line}-{line} {kind} {label}"));
                }
            }
        }
        rows
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        /// Equal inputs give byte-equal output, and rows equal an independent reference.
        #[test]
        fn symbol_determinism_property(
            files in proptest::collection::vec(
                proptest::collection::vec((any::<bool>(), 0_usize..2), 1..6), 1..4),
            name in 0_usize..2,
            form in 0_usize..3,
        ) {
            let dir = tempfile::tempdir().unwrap();
            for (file, items) in files.iter().enumerate() {
                let source: String = items
                    .iter()
                    .map(|&(in_mod, name)| {
                        if in_mod { format!("mod m {{ fn {}() {{}} }}\n", NAMES[name]) } else { format!("fn {}() {{}}\n", NAMES[name]) }
                    })
                    .collect();
                write(dir.path(), &format!("f{file}.rs"), source.as_bytes());
            }
            let (pattern, absolute, segments) = match form {
                0 => (NAMES[name].to_owned(), false, vec![NAMES[name]]),
                1 => (format!("m::{}", NAMES[name]), false, vec!["m", NAMES[name]]),
                _ => (format!("::{}", NAMES[name]), true, vec![NAMES[name]]),
            };
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let engine = engine(true, None);
            let first = runtime.block_on(search(&engine, dir.path(), &query(&pattern))).unwrap();
            let second = runtime.block_on(search(&engine, dir.path(), &query(&pattern))).unwrap();
            prop_assert_eq!(&first, &second);
            let expected = reference_rows(&files, absolute, &segments);
            match expected.as_slice() {
                [] => prop_assert!(first.starts_with("No definitions match "), "{}", first),
                [only] => prop_assert!(first.starts_with(&format!("{only} tag ")), "{}", first),
                many => {
                    let rows: Vec<&str> = first.lines().take(many.len()).collect();
                    prop_assert_eq!(rows, many.iter().map(String::as_str).collect::<Vec<_>>());
                }
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// `name[k]` addresses the k-th equal name in source order; out of range matches nothing.
        #[test]
        fn symbol_ordinal_soundness(names in proptest::collection::vec(0_usize..3, 1..12), pick in 0_usize..3, k in 1_u32..6) {
            let words = ["alpha_fn", "beta_fn", "gamma_fn"];
            let source: String = names.iter().fold(String::new(), |mut source, &name| {
                let _ = std::fmt::Write::write_fmt(&mut source, format_args!("fn {}() {{}}\n", words[name]));
                source
            });
            let lines: Vec<usize> = names
                .iter()
                .enumerate()
                .filter(|&(_, &name)| name == pick)
                .map(|(at, _)| at + 1)
                .collect();
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), "f.rs", source.as_bytes());
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let pattern = format!("{}[{k}]", words[pick]);
            let text = runtime.block_on(search(&engine(true, None), dir.path(), &query(&pattern))).unwrap();
            if let Some(line) = lines.get(usize::try_from(k).unwrap() - 1) {
                let marker = if k > 1 { format!("[{k}]") } else { String::new() };
                let header = format!("f.rs:{line}-{line} function {}{marker} tag ", words[pick]);
                prop_assert!(text.starts_with(&header), "{}", text);
            } else {
                let empty = format!("No definitions match {pattern} in the workspace.");
                prop_assert!(text.starts_with(&empty), "{}", text);
            }
        }
    }
}
