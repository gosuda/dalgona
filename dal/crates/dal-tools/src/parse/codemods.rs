//! The two codemod query seams: compiled verbatim queries over cached parses.

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use tree_sitter::{Query, QueryCursor, QueryCursorOptions, QueryCursorState, StreamingIterator};

use super::walk::text;
use super::{GATE, Language, PARSE_BUDGET_MS, ParseFailure, language, load, lock};

/// A full syntax node matched by a codemod query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodemodMatch {
    /// The language the query ran in.
    pub language: Language,
    /// Byte offset where the matched node starts.
    pub byte_start: usize,
    /// Byte offset one past the matched node's end.
    pub byte_end: usize,
    /// The matched node's source text.
    pub text: Box<str>,
}

/// Comments that read as commented-out code, as full comment nodes in source order.
///
/// One compiled query per grammar, the amendment's S-expressions and `#match?`
/// Rust-regex predicates verbatim; the whole-line rule stays with the consumer.
/// Serves `OCaml`, Python, TypeScript and JavaScript, Go, C, and C++. Rust has
/// no entry (its grammar has no shared `comment` node), and neither has a path
/// with no grammar: both return no matches.
///
/// This function blocks: it can wait on the parse gate and parse for up to
/// [`PARSE_BUDGET_MS`] on the calling thread. Async callers wrap it in
/// `spawn_blocking`.
///
/// # Errors
///
/// [`ParseFailure`] when the parse is unusable.
pub fn codemod_delete_commented_code(
    path: &Path,
    bytes: &[u8],
) -> Result<Vec<CodemodMatch>, ParseFailure> {
    let Some(lang) = language(path).filter(|lang| *lang != Language::Rust) else {
        return Ok(Vec::new());
    };
    run_query(Codemod::CommentedCode, lang, path, bytes)
}

/// Empty broad catch handlers, as full handler nodes in source order.
///
/// One compiled query per grammar, the amendment's S-expressions and `#match?`
/// Rust-regex predicates verbatim. Serves Python (`except:` and
/// `except Exception:` with an empty or `pass` body), C++ (`catch (...) {}`),
/// and `OCaml` (a `_ -> ()` arm of a `try`). Every other language returns no
/// matches.
///
/// This function blocks: it can wait on the parse gate and parse for up to
/// [`PARSE_BUDGET_MS`] on the calling thread. Async callers wrap it in
/// `spawn_blocking`.
///
/// # Errors
///
/// [`ParseFailure`] when the parse is unusable.
pub fn codemod_rethrow_empty_catch(
    path: &Path,
    bytes: &[u8],
) -> Result<Vec<CodemodMatch>, ParseFailure> {
    let Some(lang) = language(path)
        .filter(|lang| matches!(lang, Language::Python | Language::Cpp | Language::Ocaml))
    else {
        return Ok(Vec::new());
    };
    run_query(Codemod::EmptyCatch, lang, path, bytes)
}

/// The two codemod families.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Codemod {
    /// `delete-commented-code`.
    CommentedCode,
    /// `rethrow-empty-catch`.
    EmptyCatch,
}

impl Codemod {
    /// The name every pattern of this query captures.
    pub(super) fn capture(self) -> &'static str {
        match self {
            Self::CommentedCode => "dead",
            Self::EmptyCatch => "empty",
        }
    }

    /// The query source for `lang`, or `None` when this codemod has no entry.
    pub(super) fn source(self, lang: Language) -> Option<&'static str> {
        match self {
            Self::CommentedCode => match lang {
                Language::Ocaml | Language::OcamlInterface => Some(OCAML_DEAD_COMMENT),
                Language::Python => Some(PYTHON_DEAD_COMMENT),
                Language::JavaScript | Language::TypeScript | Language::Tsx => {
                    Some(SCRIPT_DEAD_COMMENT)
                }
                Language::Go => Some(GO_DEAD_COMMENT),
                Language::C | Language::Cpp => Some(C_DEAD_COMMENT),
                Language::Rust => None,
            },
            Self::EmptyCatch => match lang {
                Language::Python => Some(PYTHON_EMPTY_CATCH),
                Language::Cpp => Some(CPP_EMPTY_CATCH),
                Language::Ocaml => Some(OCAML_EMPTY_CATCH),
                _ => None,
            },
        }
    }
}

/// The codemod queries, verbatim. The doubled backslashes are required: the
/// query compiler consumes one level (`\\(` gives the regex `\(`), and the
/// `#match?` patterns run on tree-sitter's own Rust-regex engine.
pub(super) const OCAML_DEAD_COMMENT: &str = r#"((comment) @dead
  (#match? @dead "^\\(\\*\\s*(let |type |open |match |try |if |for |while )"))"#;
pub(super) const PYTHON_DEAD_COMMENT: &str = r#"((comment) @dead
  (#match? @dead "^#\\s*(def |class |if |elif |for |while |return |import |from |print\\()"))"#;
pub(super) const SCRIPT_DEAD_COMMENT: &str = r#"((comment) @dead
  (#match? @dead "^(//|/\\*)\\s*(const |let |var |function |class |if |for |while |return |import |export )"))"#;
pub(super) const GO_DEAD_COMMENT: &str = r#"((comment) @dead
  (#match? @dead "^(//|/\\*)\\s*(func |type |var |const |if |for |switch |select |return |import )"))"#;
pub(super) const C_DEAD_COMMENT: &str = r#"((comment) @dead
  (#match? @dead "^(//|/\\*)\\s*(int |void |char |bool |float |double |if |for |while |switch |return |struct |class )"))"#;
pub(super) const PYTHON_EMPTY_CATCH: &str = r#"((except_clause) @empty
  (#match? @empty "^except\\s*:\\s*(pass)?\\s*$"))
((except_clause) @empty
  (#match? @empty "^except\\s+Exception(\\s+as\\s+\\w+)?\\s*:\\s*(pass)?\\s*$"))"#;
pub(super) const CPP_EMPTY_CATCH: &str = r#"((catch_clause) @empty
  (#match? @empty "^catch\\s*\\(\\s*\\.\\.\\.\\s*\\)\\s*\\{\\s*\\}\\s*$"))"#;
pub(super) const OCAML_EMPTY_CATCH: &str = r#"((try_expression (_) (match_case) @empty)
  (#match? @empty "^\\|?\\s*_\\s*->\\s*\\(\\s*\\)\\s*$"))"#;

/// Cached compiled queries per codemod and language.
type QueryCache = HashMap<(Codemod, Language), Arc<Query>>;
static QUERY_CACHE: LazyLock<Mutex<QueryCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn compiled_query(codemod: Codemod, lang: Language) -> Result<Arc<Query>, ParseFailure> {
    let mut cache = lock(&QUERY_CACHE);
    if let Some(query) = cache.get(&(codemod, lang)) {
        return Ok(Arc::clone(query));
    }
    let source = codemod.source(lang).ok_or(ParseFailure::Panic)?;
    let query = Arc::new(Query::new(&lang.grammar(), source).map_err(|_| ParseFailure::Panic)?);
    cache.insert((codemod, lang), Arc::clone(&query));
    Ok(query)
}

/// Parses `bytes`, runs the codemod's compiled query, and collects each
/// capture's full node. The query scan is CPU work under the gate and its own
/// [`PARSE_BUDGET_MS`], started only once a slot is held — gate waits never
/// eat the budget, exactly like the parse.
fn run_query(
    codemod: Codemod,
    lang: Language,
    path: &Path,
    bytes: &[u8],
) -> Result<Vec<CodemodMatch>, ParseFailure> {
    let (parsed, permit) = load(path, bytes)?;
    let _admitted = match permit {
        Some(permit) => permit,
        None => GATE.enter()?,
    };
    let query = compiled_query(codemod, lang)?;
    let capture = query
        .capture_index_for_name(codemod.capture())
        .ok_or(ParseFailure::Panic)?;
    let started = Instant::now();
    let budget = Duration::from_millis(PARSE_BUDGET_MS);
    let expired = AtomicBool::new(false);
    let mut on_progress = |_: &QueryCursorState| {
        if started.elapsed() >= budget {
            expired.store(true, Ordering::Relaxed);
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = QueryCursorOptions::new().progress_callback(&mut on_progress);
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches_with_options(&query, parsed.tree.root_node(), bytes, options);
    let mut found = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    while let Some(matched) = matches.next() {
        for node in matched.nodes_for_capture_index(capture) {
            // One pattern can match a capture through several sibling
            // choices, and the iterator does not dedupe: one node, one match.
            if seen.insert(node.id()) {
                found.push(CodemodMatch {
                    language: lang,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    text: text(node, bytes).into(),
                });
            }
        }
    }
    if expired.load(Ordering::Relaxed) {
        return Err(ParseFailure::Budget);
    }
    Ok(found)
}
