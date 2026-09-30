//! The parser service: grammar selection by extension, bounded tree-sitter
//! parses admitted by a CPU gate, one process-wide parse cache keyed by
//! content, hand-written definition walks, and the two codemod query seams.
//!
//! Every parse runs under a per-file budget. An expired, refused, or panicked
//! parse is a typed [`ParseFailure`]; callers treat it as unusable, never as an
//! empty answer. A syntax error is not a failure: the tree carries `ERROR` or
//! `MISSING` nodes and [`Parsed::error_count`] counts them.

use std::collections::HashMap;
use std::num::NonZero;
use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, PoisonError};
use std::thread::available_parallelism;
use std::time::{Duration, Instant};

use tree_sitter::{ParseOptions, ParseState, Parser, Point, Tree};

mod codemods;
mod walk;

pub use codemods::{CodemodMatch, codemod_delete_commented_code, codemod_rethrow_empty_catch};

/// Wall-clock budget of one parse, in milliseconds.
pub const PARSE_BUDGET_MS: u64 = 250;
/// Upper bound on parses running at once; the gate also caps at the core count.
pub const MAX_PARSSES_IN_FLIGHT: usize = 32;
/// Parses allowed to wait for a slot; one more fails with [`ParseFailure::TooManyParses`].
pub const MAX_PARSE_QUEUE: usize = 100;
/// Parsed trees the process-wide cache keeps.
pub const MAX_CACHED_TREES: usize = 256;
/// Largest source, in bytes, whose parse enters the cache.
pub const MAX_CACHED_SOURCE_BYTES: usize = 4 << 20;

/// A language with a grammar and a definition walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    /// Rust, `.rs`.
    Rust,
    /// `OCaml` implementation, `.ml`.
    Ocaml,
    /// `OCaml` interface, `.mli`.
    OcamlInterface,
    /// Python, `.py`.
    Python,
    /// JavaScript, `.js`, `.mjs`, `.cjs`.
    JavaScript,
    /// TypeScript, `.ts`.
    TypeScript,
    /// TypeScript with JSX, `.tsx`.
    Tsx,
    /// Go, `.go`.
    Go,
    /// C, `.c`.
    C,
    /// C++, `.cc`, `.cpp`, `.cxx`, `.hpp`, `.hh`, `.hxx`.
    Cpp,
}

impl Language {
    /// The pinned tree-sitter grammar for this language.
    #[must_use]
    // Grammar constants and `.into()` per the pinned bindings, e.g.
    // https://raw.githubusercontent.com/tree-sitter/tree-sitter-ocaml/v0.26.0/bindings/rust/lib.rs
    pub fn grammar(self) -> tree_sitter::Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Ocaml => tree_sitter_ocaml::LANGUAGE_OCAML.into(),
            Self::OcamlInterface => tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        }
    }
}

/// The language of `path` by its extension, or `None` when no grammar serves it.
#[must_use]
pub fn language(path: &Path) -> Option<Language> {
    Some(match path.extension()?.to_str()? {
        "rs" => Language::Rust,
        "ml" => Language::Ocaml,
        "mli" => Language::OcamlInterface,
        "py" => Language::Python,
        "js" | "mjs" | "cjs" => Language::JavaScript,
        "ts" => Language::TypeScript,
        "tsx" => Language::Tsx,
        "go" => Language::Go,
        "c" => Language::C,
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Language::Cpp,
        _ => return None,
    })
}

/// The closed set of definition kinds every language folds onto.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A free function.
    Function,
    /// A function inside a class, impl, trait, or receiver type.
    Method,
    /// A struct or union.
    Struct,
    /// An enumeration.
    Enum,
    /// A Rust trait.
    Trait,
    /// A Rust impl block, named by its type.
    Impl,
    /// A module or namespace.
    Module,
    /// A macro.
    Macro,
    /// A constant or immutable top-level value.
    Const,
    /// A type alias or type definition.
    Type,
    /// A class.
    Class,
    /// An interface or module type.
    Interface,
}

impl Kind {
    /// The lowercase kind string shown to the model.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Module => "module",
            Self::Macro => "macro",
            Self::Const => "const",
            Self::Type => "type",
            Self::Class => "class",
            Self::Interface => "interface",
        }
    }
}

/// One definition found by the walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Def {
    /// The bare name.
    pub name: Box<str>,
    /// The enclosing definitions' names and this name, joined with `::`.
    pub qualified: Box<str>,
    /// 1-based position among the file's definitions with the same name, in source order.
    pub ordinal: u32,
    /// The folded kind.
    pub kind: Kind,
    /// First line, 1-based.
    pub first: u32,
    /// Last line, 1-based and inclusive.
    pub last: u32,
    /// Byte offset where the definition starts.
    pub byte_start: usize,
    /// Byte offset one past the definition's end.
    pub byte_end: usize,
}

/// One parse: the tree, its definitions in source order, and its syntax-error count.
#[derive(Clone, Debug)]
pub struct Parsed {
    /// The grammar the bytes were parsed with.
    pub lang: Language,
    /// The syntax tree, shared with the cache.
    pub tree: Arc<Tree>,
    /// Definitions in source order, outer before inner.
    pub defs: Vec<Def>,
    /// Count of `ERROR` and `MISSING` nodes.
    pub error_count: usize,
}

/// Why a parse is unusable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseFailure {
    /// The per-file parse budget expired before the parse finished.
    #[error("parse budget reached")]
    Budget,
    /// Every parse slot is busy and the wait queue is full.
    #[error("parse queue full")]
    TooManyParses,
    /// The parser or the extractor panicked, or the grammar was rejected.
    #[error("the parser panicked")]
    Panic,
    /// No grammar serves the path's extension.
    #[error("no grammar for this file")]
    Unsupported,
}

/// Parses `bytes` as the file at `path`, reusing the cached parse of equal bytes.
///
/// # Errors
///
/// [`ParseFailure`] when the parse is unusable or no grammar serves `path`.
pub async fn tree(path: &Path, bytes: &[u8]) -> Result<Parsed, ParseFailure> {
    with_parse(path, bytes, Parsed::clone).await
}

/// The definitions of `bytes` as the file at `path`, in source order.
///
/// # Errors
///
/// [`ParseFailure`] when the parse is unusable or no grammar serves `path`.
pub async fn definitions(path: &Path, bytes: &[u8]) -> Result<Vec<Def>, ParseFailure> {
    with_parse(path, bytes, |parsed| parsed.defs.clone()).await
}

/// The outermost named node starting on 1-based `line`: byte span, first line, last line.
///
/// # Errors
///
/// [`ParseFailure`] when the parse is unusable or no grammar serves `path`.
pub async fn node_at(
    path: &Path,
    bytes: &[u8],
    line: u32,
) -> Result<Option<(usize, usize, u32, u32)>, ParseFailure> {
    with_parse(path, bytes, move |parsed| outermost_at(&parsed.tree, line)).await
}

/// The count of `ERROR` and `MISSING` nodes in the parse of `bytes`.
///
/// # Errors
///
/// [`ParseFailure`] when the parse is unusable or no grammar serves `path`.
pub async fn errors(path: &Path, bytes: &[u8]) -> Result<usize, ParseFailure> {
    with_parse(path, bytes, |parsed| parsed.error_count).await
}

async fn with_parse<T, F>(path: &Path, bytes: &[u8], read: F) -> Result<T, ParseFailure>
where
    T: Send + 'static,
    F: FnOnce(&Parsed) -> T + Send + 'static,
{
    let path = path.to_path_buf();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || -> Result<T, ParseFailure> {
        let (parsed, _permit) = load(&path, &bytes)?;
        Ok(read(&parsed))
    })
    .await
    .unwrap_or(Err(ParseFailure::Panic))
}

fn load(path: &Path, bytes: &[u8]) -> Result<(Arc<Parsed>, Option<Permit<'static>>), ParseFailure> {
    let lang = language(path).ok_or(ParseFailure::Unsupported)?;
    let key = (canonical(path), crate::digest32(bytes));
    // A hit spent no parse CPU, so it takes no gate slot: `None` sends the
    // query phase to the gate itself, and async readers stay queue-free.
    let hit = lock(&CACHE).get(&key);
    if let Some(parsed) = hit {
        return Ok((parsed, None));
    }
    let permit = GATE.enter()?;
    let parsed = Arc::new(parse_fresh(lang, &key.0, bytes)?);
    if bytes.len() <= MAX_CACHED_SOURCE_BYTES {
        lock(&CACHE).put(key, Arc::clone(&parsed));
    }
    Ok((parsed, Some(permit)))
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn parse_fresh(lang: Language, canonical: &Path, bytes: &[u8]) -> Result<Parsed, ParseFailure> {
    let budget = budget_for(canonical);
    catch_unwind(AssertUnwindSafe(|| {
        #[cfg(test)]
        hooks::note_parse(canonical);
        let mut parser = Parser::new();
        parser
            .set_language(&lang.grammar())
            .map_err(|_| ParseFailure::Panic)?;
        let started = Instant::now();
        // The progress callback's `Break` cancels the parse and makes
        // `parse_with_options` return `None`; 0.27 has no `set_timeout_micros`. See
        // https://docs.rs/tree-sitter/0.27.0/tree_sitter/struct.ParseOptions.html
        let mut expired = |_: &ParseState| {
            if started.elapsed() >= budget {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut input = |offset: usize, _: Point| bytes.get(offset..).unwrap_or_default();
        let options = ParseOptions::new().progress_callback(&mut expired);
        let tree = parser
            .parse_with_options(&mut input, None, Some(options))
            .ok_or(ParseFailure::Budget)?;
        #[cfg(test)]
        hooks::before_extract(canonical);
        let (defs, error_count) = walk::extract(lang, &tree, bytes);
        Ok(Parsed {
            lang,
            tree: Arc::new(tree),
            defs,
            error_count,
        })
    }))
    .unwrap_or(Err(ParseFailure::Panic))
}

#[cfg(not(test))]
fn budget_for(_: &Path) -> Duration {
    Duration::from_millis(PARSE_BUDGET_MS)
}

#[cfg(test)]
fn budget_for(canonical: &Path) -> Duration {
    hooks::budget(canonical).unwrap_or(Duration::from_millis(PARSE_BUDGET_MS))
}

type CacheKey = (PathBuf, [u8; 32]);

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(Cache::default()));

/// Least-recently-used map of parses keyed by canonical path and content digest.
#[derive(Default)]
struct Cache {
    tick: u64,
    entries: HashMap<CacheKey, (u64, Arc<Parsed>)>,
}

impl Cache {
    fn get(&mut self, key: &CacheKey) -> Option<Arc<Parsed>> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(key).map(|(used, parsed)| {
            *used = tick;
            Arc::clone(parsed)
        })
    }

    fn put(&mut self, key: CacheKey, parsed: Arc<Parsed>) {
        self.tick += 1;
        self.entries.insert(key, (self.tick, parsed));
        if self.entries.len() > MAX_CACHED_TREES {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
    }
}

static GATE: LazyLock<Gate> = LazyLock::new(|| {
    let cores = available_parallelism().map_or(1, NonZero::get);
    Gate::new(cores.min(MAX_PARSSES_IN_FLIGHT), MAX_PARSE_QUEUE)
});

/// Counting semaphore with a bounded wait queue that fails fast when full.
struct Gate {
    slots: usize,
    queue: usize,
    state: Mutex<GateState>,
    freed: Condvar,
}

#[derive(Default)]
struct GateState {
    running: usize,
    waiting: usize,
    #[cfg(test)]
    peak: usize,
}

struct Permit<'g>(&'g Gate);

impl Gate {
    fn new(slots: usize, queue: usize) -> Self {
        Self {
            slots,
            queue,
            state: Mutex::new(GateState::default()),
            freed: Condvar::new(),
        }
    }

    fn enter(&self) -> Result<Permit<'_>, ParseFailure> {
        let mut state = lock(&self.state);
        if state.running >= self.slots {
            if state.waiting >= self.queue {
                return Err(ParseFailure::TooManyParses);
            }
            state.waiting += 1;
            while state.running >= self.slots {
                state = self
                    .freed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            state.waiting -= 1;
        }
        state.running += 1;
        #[cfg(test)]
        {
            state.peak = state.peak.max(state.running);
        }
        Ok(Permit(self))
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.running -= 1;
        drop(state);
        self.0.freed.notify_one();
    }
}

fn outermost_at(tree: &Tree, line: u32) -> Option<(usize, usize, u32, u32)> {
    let row = usize::try_from(line.checked_sub(1)?).ok()?;
    let mut cursor = tree.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        let node = cursor.node();
        let start = node.start_position().row;
        if start > row {
            return None;
        }
        if start == row && node.is_named() && !node.is_error() && !node.is_missing() {
            let (first, last) = walk::lines(node);
            return Some((node.start_byte(), node.end_byte(), first, last));
        }
        if node.end_position().row >= row && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return None;
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod hooks {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{LazyLock, Mutex};
    use std::time::Duration;

    use super::{CACHE, GATE, canonical, lock};

    #[derive(Default)]
    struct Hook {
        budget: Option<Duration>,
        panic: bool,
        parses: u64,
    }

    static HOOKS: LazyLock<Mutex<HashMap<PathBuf, Hook>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    /// Overrides the parse budget for `path`.
    pub(crate) fn set_budget(path: &Path, budget: Duration) {
        lock(&HOOKS).entry(canonical(path)).or_default().budget = Some(budget);
    }

    /// Makes the extractor panic for `path`.
    pub(crate) fn set_panic(path: &Path) {
        lock(&HOOKS).entry(canonical(path)).or_default().panic = true;
    }

    /// Clears the budget override and the panic hook for `path`.
    pub(crate) fn reset(path: &Path) {
        if let Some(hook) = lock(&HOOKS).get_mut(&canonical(path)) {
            hook.budget = None;
            hook.panic = false;
        }
    }

    /// Parses started for `path`, cache hits excluded.
    pub(crate) fn parse_count(path: &Path) -> u64 {
        lock(&HOOKS)
            .get(&canonical(path))
            .map_or(0, |hook| hook.parses)
    }

    /// Drops every cached parse of `path`, as a fresh process would start.
    pub(crate) fn forget(path: &Path) {
        let path = canonical(path);
        lock(&CACHE)
            .entries
            .retain(|(cached, _), _| *cached != path);
    }

    /// The most parses the process-wide gate has admitted at once.
    pub(crate) fn peak_in_flight() -> usize {
        lock(&GATE.state).peak
    }

    pub(super) fn budget(canonical: &Path) -> Option<Duration> {
        lock(&HOOKS).get(canonical).and_then(|hook| hook.budget)
    }

    pub(super) fn note_parse(canonical: &Path) {
        lock(&HOOKS)
            .entry(canonical.to_path_buf())
            .or_default()
            .parses += 1;
    }

    pub(super) fn before_extract(canonical: &Path) {
        let panic = lock(&HOOKS).get(canonical).is_some_and(|hook| hook.panic);
        if panic {
            std::panic::panic_any("injected extractor panic");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::codemods::{Codemod, CodemodMatch};
    use super::*;
    use tree_sitter::Query;

    const RUST: &str = "\
fn alpha() {}
struct Beta {
    x: u32,
}
enum Gamma { A }
trait Delta {
    fn sig(&self);
}
impl Delta for Beta {
    fn sig(&self) {}
}
mod epsilon {
    pub fn inner() {}
}
macro_rules! zeta { () => {}; }
const ETA: u32 = 1;
static THETA: u32 = 2;
type Iota = u32;
";

    fn rows(defs: &[Def]) -> Vec<(&str, &str, u32, u32, u32)> {
        defs.iter()
            .map(|def| {
                (
                    def.kind.as_str(),
                    &*def.qualified,
                    def.first,
                    def.last,
                    def.ordinal,
                )
            })
            .collect()
    }

    async fn defs_of(path: &str, src: &str) -> Vec<Def> {
        definitions(Path::new(path), src.as_bytes()).await.unwrap()
    }

    fn assert_spans_hold_names(src: &str, defs: &[Def]) {
        for def in defs {
            let span = &src[def.byte_start..def.byte_end];
            assert!(span.contains(&*def.name), "{span:?} lacks {}", def.name);
            assert_eq!(def.qualified.rsplit("::").next(), Some(&*def.name));
        }
    }

    #[test]
    fn extension_table_is_closed() {
        let table = [
            ("a.rs", Language::Rust),
            ("a.ml", Language::Ocaml),
            ("a.mli", Language::OcamlInterface),
            ("a.py", Language::Python),
            ("a.js", Language::JavaScript),
            ("a.mjs", Language::JavaScript),
            ("a.cjs", Language::JavaScript),
            ("a.ts", Language::TypeScript),
            ("a.tsx", Language::Tsx),
            ("a.go", Language::Go),
            ("a.c", Language::C),
            ("a.cc", Language::Cpp),
            ("a.cpp", Language::Cpp),
            ("a.cxx", Language::Cpp),
            ("a.hpp", Language::Cpp),
            ("a.hh", Language::Cpp),
            ("a.hxx", Language::Cpp),
        ];
        for (path, lang) in table {
            assert_eq!(language(Path::new(path)), Some(lang), "{path}");
        }
        for path in ["a.h", "a.jsx", "a.RS", "Makefile", "a.txt", "rs"] {
            assert_eq!(language(Path::new(path)), None, "{path}");
        }
    }

    #[test]
    fn kind_strings_are_the_closed_twelve() {
        let kinds = [
            Kind::Function,
            Kind::Method,
            Kind::Struct,
            Kind::Enum,
            Kind::Trait,
            Kind::Impl,
            Kind::Module,
            Kind::Macro,
            Kind::Const,
            Kind::Type,
            Kind::Class,
            Kind::Interface,
        ];
        let strings: Vec<&str> = kinds.iter().map(|kind| kind.as_str()).collect();
        assert_eq!(
            strings,
            [
                "function",
                "method",
                "struct",
                "enum",
                "trait",
                "impl",
                "module",
                "macro",
                "const",
                "type",
                "class",
                "interface"
            ]
        );
    }

    #[test]
    fn grammar_abi_matches_runtime() {
        let langs = [
            Language::Rust,
            Language::Ocaml,
            Language::OcamlInterface,
            Language::Python,
            Language::JavaScript,
            Language::TypeScript,
            Language::Tsx,
            Language::Go,
            Language::C,
            Language::Cpp,
        ];
        for lang in langs {
            let mut parser = Parser::new();
            assert!(parser.set_language(&lang.grammar()).is_ok(), "{lang:?}");
        }
    }

    #[tokio::test]
    async fn rust_kinds_and_spans() {
        let defs = defs_of("fixture.rs", RUST).await;
        assert_eq!(
            rows(&defs),
            [
                ("function", "alpha", 1, 1, 1),
                ("struct", "Beta", 2, 4, 1),
                ("enum", "Gamma", 5, 5, 1),
                ("trait", "Delta", 6, 8, 1),
                ("method", "sig", 7, 7, 1),
                ("impl", "Beta", 9, 11, 2),
                ("method", "Beta::sig", 10, 10, 2),
                ("module", "epsilon", 12, 14, 1),
                ("function", "epsilon::inner", 13, 13, 1),
                ("macro", "zeta", 15, 15, 1),
                ("const", "ETA", 16, 16, 1),
                ("const", "THETA", 17, 17, 1),
                ("type", "Iota", 18, 18, 1),
            ]
        );
        assert_eq!(
            (defs[0].byte_start, defs[0].byte_end),
            (0, "fn alpha() {}".len())
        );
        assert_spans_hold_names(RUST, &defs);
    }

    #[tokio::test]
    async fn python_methods_and_ordinals() {
        let src = "\
class Box:
    def open(self):
        pass

    def open(self):
        return 1

def open():
    pass
";
        let defs = defs_of("fixture.py", src).await;
        assert_eq!(
            rows(&defs),
            [
                ("class", "Box", 1, 6, 1),
                ("method", "Box::open", 2, 3, 1),
                ("method", "Box::open", 5, 6, 2),
                ("function", "open", 8, 9, 3),
            ]
        );
        assert_spans_hold_names(src, &defs);
    }

    #[tokio::test]
    async fn typescript_kinds() {
        let src = "\
function f() {}
class C {
  m() {}
}
interface I {}
enum E { A }
const K = 1;
type T = number;
namespace N {}
";
        let defs = defs_of("fixture.ts", src).await;
        assert_eq!(
            rows(&defs),
            [
                ("function", "f", 1, 1, 1),
                ("class", "C", 2, 4, 1),
                ("method", "C::m", 3, 3, 1),
                ("interface", "I", 5, 5, 1),
                ("enum", "E", 6, 6, 1),
                ("const", "K", 7, 7, 1),
                ("type", "T", 8, 8, 1),
                ("module", "N", 9, 9, 1),
            ]
        );
        assert_spans_hold_names(src, &defs);
    }

    #[tokio::test]
    async fn go_kinds() {
        let src = "\
package p

func Run() {}

type Engine struct{}

func (e *Engine) Start() {}

type Runner interface{ Run() }

type ID = int

const Max = 3
";
        let defs = defs_of("fixture.go", src).await;
        assert_eq!(
            rows(&defs),
            [
                ("function", "Run", 3, 3, 1),
                ("struct", "Engine", 5, 5, 1),
                ("method", "Engine::Start", 7, 7, 1),
                ("interface", "Runner", 9, 9, 1),
                ("type", "ID", 11, 11, 1),
                ("const", "Max", 13, 13, 1),
            ]
        );
        assert!(src[defs[1].byte_start..].starts_with("type Engine"));
        assert_spans_hold_names(src, &defs);
    }

    #[tokio::test]
    async fn ocaml_kinds() {
        let src = "\
let add x y = x + y
let limit = 10
type shape = Circle
module M = struct
  let inner () = ()
end
exception Boom
";
        let defs = defs_of("fixture.ml", src).await;
        assert_eq!(
            rows(&defs),
            [
                ("function", "add", 1, 1, 1),
                ("const", "limit", 2, 2, 1),
                ("type", "shape", 3, 3, 1),
                ("module", "M", 4, 6, 1),
                ("function", "M::inner", 5, 5, 1),
                ("type", "Boom", 7, 7, 1),
            ]
        );
        assert!(src[defs[0].byte_start..].starts_with("let add"));
        assert_spans_hold_names(src, &defs);

        let interface = defs_of("fixture.mli", "val f : int -> int\nval n : int\n").await;
        assert_eq!(
            rows(&interface),
            [("function", "f", 1, 1, 1), ("const", "n", 2, 2, 1)]
        );
    }

    #[tokio::test]
    async fn c_and_cpp_kinds() {
        let c = "\
#define LIMIT 3
struct point { int x; };
typedef int count;
int main(void) { return 0; }
";
        let defs = defs_of("fixture.c", c).await;
        assert_eq!(
            rows(&defs),
            [
                ("macro", "LIMIT", 1, 1, 1),
                ("struct", "point", 2, 2, 1),
                ("type", "count", 3, 3, 1),
                ("function", "main", 4, 4, 1),
            ]
        );
        assert_spans_hold_names(c, &defs);

        let cpp = "\
namespace ns {
class Widget {
  void draw() {}
};
}
void Widget::paint() {}
";
        let defs = defs_of("fixture.cpp", cpp).await;
        assert_eq!(
            rows(&defs),
            [
                ("module", "ns", 1, 5, 1),
                ("class", "ns::Widget", 2, 4, 1),
                ("method", "ns::Widget::draw", 3, 3, 1),
                ("method", "Widget::paint", 6, 6, 1),
            ]
        );
        assert_spans_hold_names(cpp, &defs);
    }

    #[tokio::test]
    async fn syntax_errors_keep_found_definitions() {
        let src = "fn good() {}\nfn broken( {\n";
        let parsed = tree(Path::new("broken.rs"), src.as_bytes()).await.unwrap();
        assert!(parsed.error_count > 0);
        assert!(parsed.defs.iter().any(|def| &*def.name == "good"));
        assert_eq!(
            errors(Path::new("broken.rs"), src.as_bytes()).await,
            Ok(parsed.error_count)
        );
        assert_eq!(errors(Path::new("clean.rs"), RUST.as_bytes()).await, Ok(0));
    }

    #[tokio::test]
    async fn unsupported_extension_is_typed() {
        assert_eq!(
            definitions(Path::new("notes.txt"), b"fn a() {}").await,
            Err(ParseFailure::Unsupported)
        );
    }

    #[tokio::test]
    async fn node_at_finds_the_outermost_node() {
        let path = Path::new("node_at.rs");
        let impl_start = RUST.find("impl Delta").unwrap();
        let impl_end = RUST.find("mod epsilon").unwrap() - 1;
        assert_eq!(
            node_at(path, RUST.as_bytes(), 9).await,
            Ok(Some((impl_start, impl_end, 9, 11)))
        );
        let field = RUST.find("x: u32").unwrap();
        let found = node_at(path, RUST.as_bytes(), 3).await.unwrap().unwrap();
        assert_eq!((found.0, found.2, found.3), (field, 3, 3));
        assert_eq!(node_at(path, RUST.as_bytes(), 0).await, Ok(None));
        assert_eq!(node_at(path, RUST.as_bytes(), 100).await, Ok(None));
    }

    #[tokio::test]
    async fn cache_keys_on_path_and_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cached.rs");
        std::fs::write(&path, RUST).unwrap();
        tree(&path, RUST.as_bytes()).await.unwrap();
        assert_eq!(hooks::parse_count(&path), 1);
        tree(&path, RUST.as_bytes()).await.unwrap();
        assert_eq!(hooks::parse_count(&path), 1, "equal bytes hit");
        let changed = format!("{RUST}fn omega() {{}}\n");
        let defs = definitions(&path, changed.as_bytes()).await.unwrap();
        assert_eq!(hooks::parse_count(&path), 2, "changed bytes re-parse");
        assert_eq!(defs.last().map(|def| &*def.name), Some("omega"));
        hooks::forget(&path);
        tree(&path, RUST.as_bytes()).await.unwrap();
        assert_eq!(hooks::parse_count(&path), 3, "a fresh process re-parses");
    }

    #[test]
    fn cache_evicts_least_recently_used() {
        let tree = {
            let mut parser = Parser::new();
            parser.set_language(&Language::Rust.grammar()).unwrap();
            Arc::new(parser.parse("", None).unwrap())
        };
        let parsed = Arc::new(Parsed {
            lang: Language::Rust,
            tree,
            defs: Vec::new(),
            error_count: 0,
        });
        let key = |n: usize| (PathBuf::from(format!("{n}.rs")), [0; 32]);
        let mut cache = Cache::default();
        for n in 0..MAX_CACHED_TREES {
            cache.put(key(n), Arc::clone(&parsed));
        }
        assert!(cache.get(&key(0)).is_some());
        cache.put(key(MAX_CACHED_TREES), Arc::clone(&parsed));
        assert_eq!(cache.entries.len(), MAX_CACHED_TREES);
        assert!(cache.get(&key(0)).is_some(), "recently used survives");
        assert!(
            cache.get(&key(1)).is_none(),
            "least recently used is evicted"
        );
    }

    #[tokio::test]
    async fn expired_budget_is_unusable_and_uncached() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.c");
        let src: String = (0..8_000)
            .map(|n| format!("int f{n}(int x) {{ return x + {n}; }}\n"))
            .collect();
        hooks::set_budget(&path, Duration::ZERO);
        assert_eq!(
            definitions(&path, src.as_bytes()).await,
            Err(ParseFailure::Budget)
        );
        hooks::set_budget(&path, Duration::from_secs(60));
        let defs = definitions(&path, src.as_bytes()).await.unwrap();
        hooks::reset(&path);
        assert_eq!(defs.len(), 8_000);
        assert_eq!(
            hooks::parse_count(&path),
            2,
            "the expired parse was not cached"
        );
    }

    #[tokio::test]
    async fn extractor_panic_is_contained() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.rs");
        let good = dir.path().join("good.rs");
        hooks::set_panic(&bad);
        assert_eq!(
            definitions(&bad, RUST.as_bytes()).await,
            Err(ParseFailure::Panic)
        );
        assert!(definitions(&good, RUST.as_bytes()).await.is_ok());
    }

    #[test]
    fn admission_queue_overflow_fails_fast() {
        let gate = Gate::new(1, 1);
        let held = gate.enter().unwrap();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| gate.enter().map(drop).is_ok());
            while lock(&gate.state).waiting < 1 {
                std::thread::yield_now();
            }
            assert!(matches!(gate.enter(), Err(ParseFailure::TooManyParses)));
            drop(held);
            assert!(waiter.join().unwrap());
        });
        let state = lock(&gate.state);
        assert_eq!((state.running, state.waiting, state.peak), (0, 0, 1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_parses_respect_the_slot_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut set = tokio::task::JoinSet::new();
        for n in 0..64 {
            let path = dir.path().join(format!("f{n}.rs"));
            set.spawn(async move { definitions(&path, RUST.as_bytes()).await });
        }
        while let Some(result) = set.join_next().await {
            assert_eq!(result.unwrap().map(|defs| defs.len()), Ok(13));
        }
        assert!(hooks::peak_in_flight() <= GATE.slots);
    }

    fn texts(found: &[CodemodMatch]) -> Vec<&str> {
        found.iter().map(|m| &*m.text).collect()
    }

    #[test]
    fn delete_commented_code_matches_code_comments_only() {
        let py = "x = 1\n# def unused(): pass\n# this is prose\n";
        let found = codemod_delete_commented_code(Path::new("a.py"), py.as_bytes()).unwrap();
        assert_eq!(texts(&found), ["# def unused(): pass"]);
        assert_eq!(
            &py[found[0].byte_start..found[0].byte_end],
            "# def unused(): pass"
        );
        assert_eq!(found[0].language, Language::Python);

        let ml = "let g () = 1  (* let h = 2 *)\n(* this helper normalizes the input *)\n";
        let found = codemod_delete_commented_code(Path::new("a.ml"), ml.as_bytes()).unwrap();
        assert_eq!(texts(&found), ["(* let h = 2 *)"]);

        let go = "package p\n// if err != nil {\n";
        let found = codemod_delete_commented_code(Path::new("a.go"), go.as_bytes()).unwrap();
        assert_eq!(texts(&found), ["// if err != nil {"]);

        let ts = "// return null;\nconst a = 1;\n";
        let found = codemod_delete_commented_code(Path::new("a.ts"), ts.as_bytes()).unwrap();
        assert_eq!(texts(&found), ["// return null;"]);

        let cpp = "/* for (;;) { */\nint x;\n";
        let found = codemod_delete_commented_code(Path::new("a.cpp"), cpp.as_bytes()).unwrap();
        assert_eq!(texts(&found), ["/* for (;;) { */"]);

        let mli = "(* let f x = x *)\nval n : int\n";
        let found = codemod_delete_commented_code(Path::new("a.mli"), mli.as_bytes()).unwrap();
        assert_eq!(texts(&found), ["(* let f x = x *)"]);

        let rs = "// let x = compute();\nfn f() {}\n";
        assert_eq!(
            codemod_delete_commented_code(Path::new("a.rs"), rs.as_bytes()),
            Ok(Vec::new())
        );
    }

    #[test]
    fn rethrow_empty_catch_matches_broad_empty_handlers() {
        let py = "\
try:
    save()
except Exception:
    pass
try:
    save()
except ValueError:
    pass
";
        let found = codemod_rethrow_empty_catch(Path::new("a.py"), py.as_bytes()).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].text.starts_with("except Exception:"));
        assert!(found[0].text.trim_end().ends_with("pass"));

        let cpp = "\
void g() {
  try { load(); } catch (...) {}
  try { load(); } catch (const std::exception&) {}
}
";
        let found = codemod_rethrow_empty_catch(Path::new("a.cpp"), cpp.as_bytes()).unwrap();
        assert_eq!(texts(&found), ["catch (...) {}"]);
        assert_eq!(
            codemod_rethrow_empty_catch(Path::new("a.c"), cpp.as_bytes()),
            Ok(Vec::new())
        );

        let ml = "let a = try f () with _ -> ()\nlet b = match x with _ -> ()\n";
        let found = codemod_rethrow_empty_catch(Path::new("a.ml"), ml.as_bytes()).unwrap();
        assert_eq!(texts(&found), ["_ -> ()"]);

        let multi = "let r =\n  try f () with\n  | E -> ()\n  | _ -> ()\n";
        let found = codemod_rethrow_empty_catch(Path::new("b.ml"), multi.as_bytes()).unwrap();
        assert_eq!(
            found.len(),
            1,
            "one wildcard arm, not one match per sibling"
        );
    }

    #[test]
    fn codemod_queries_compile_for_every_served_language() {
        let delete = [
            (Codemod::CommentedCode, Language::Ocaml),
            (Codemod::CommentedCode, Language::OcamlInterface),
            (Codemod::CommentedCode, Language::Python),
            (Codemod::CommentedCode, Language::JavaScript),
            (Codemod::CommentedCode, Language::TypeScript),
            (Codemod::CommentedCode, Language::Tsx),
            (Codemod::CommentedCode, Language::Go),
            (Codemod::CommentedCode, Language::C),
            (Codemod::CommentedCode, Language::Cpp),
        ];
        let rethrow = [
            (Codemod::EmptyCatch, Language::Python),
            (Codemod::EmptyCatch, Language::Cpp),
            (Codemod::EmptyCatch, Language::Ocaml),
        ];
        for (codemod, lang) in delete.into_iter().chain(rethrow) {
            let source = codemod.source(lang).expect("language is served");
            let query = Query::new(&lang.grammar(), source)
                .unwrap_or_else(|error| panic!("{lang:?}: {error}"));
            let capture = codemod.capture();
            assert!(
                query.capture_index_for_name(capture).is_some(),
                "{lang:?} lacks @{capture}"
            );
        }
    }
}
