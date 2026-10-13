//! Grep mode: the mandatory-trigram plan, in-process verification, and output.
//!
//! The index only narrows the candidate files. Every candidate is re-read and
//! verified against its current bytes, so a stale index can never serve a
//! stale match. Verification uses the ripgrep crates in process:
//! <https://docs.rs/grep-searcher/0.1.17/grep_searcher/> (Searcher, Sink) and
//! <https://docs.rs/grep-regex/0.1.14/grep_regex/struct.RegexMatcherBuilder.html>.
//! The plan walks the parsed pattern:
//! <https://docs.rs/regex-syntax/0.8.11/regex_syntax/hir/index.html>.

use std::collections::{BTreeSet, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use regex_syntax::hir::{Class, Hir, HirKind};

use super::find::{self, ContentClass, FindGlob};
use super::page::{DraftFile, DraftHit, GrepDraft};
use super::{
    Call, REASON_NO_GRAM, REASON_NO_INDEX, REASON_NON_ASCII, REASON_SHORT, REASON_SINGLE_FILE,
    Scope, Search, SearchArgs, SearchError, Target, push_line, seen_key, universe,
};
use crate::{RerankCandidate, digest32};

/// Displayed line text is cut after this many characters.
const CROP_CHARS: usize = 500;
/// The largest set of exact strings tracked for one sub-pattern.
const EXACT_CAP: usize = 16;
/// The largest character class expanded into exact strings.
const CLASS_CAP: u32 = 16;
/// Most matches kept per file while reranking: bounds memory when the rerank
/// path verifies every file, while `total` still counts each match exactly.
const RERANK_LINES_CAP: usize = 10_000;

/// How a query selects the files it verifies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Ask the index: an AND of OR-groups of literal byte strings of at least 3 bytes.
    Index {
        clauses: Vec<Vec<Vec<u8>>>,
        ignore_case: bool,
    },
    /// Verify the whole universe, printing the named reason.
    Scan(&'static str),
}

/// A compiled grep query.
pub(crate) struct Query {
    matcher: RegexMatcher,
    plan: Plan,
}

/// Compiles the matcher and the candidate plan; invalid patterns fail with the exact text.
pub(crate) fn compile(
    pattern: &str,
    literal: bool,
    ignore_case: bool,
) -> Result<Query, SearchError> {
    let source = if literal {
        regex::escape(pattern)
    } else {
        pattern.to_owned()
    };
    let invalid = |reason: String| {
        SearchError::msg(format!(
            "search: invalid regular expression: {pattern}: {}",
            last_line(&reason)
        ))
    };
    regex::RegexBuilder::new(&source)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|error| invalid(error.to_string()))?;
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(ignore_case)
        .line_terminator(Some(b'\n'))
        .build(&source)
        .map_err(|error| invalid(error.to_string()))?;
    Ok(Query {
        matcher,
        plan: plan(&source, pattern, literal, ignore_case),
    })
}

/// The one-line reason of a possibly multi-line regex error.
fn last_line(text: &str) -> &str {
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
        .trim();
    line.strip_prefix("error: ").unwrap_or(line)
}

/// Plans the mandatory-trigram query of `source`, the regex form of `pattern`.
pub(crate) fn plan(source: &str, pattern: &str, literal: bool, ignore_case: bool) -> Plan {
    if literal && pattern.len() < 3 {
        return Plan::Scan(REASON_SHORT);
    }
    if ignore_case && !pattern.is_ascii() {
        return Plan::Scan(REASON_NON_ASCII);
    }
    let Ok(hir) = regex_syntax::ParserBuilder::new()
        .case_insensitive(ignore_case)
        .build()
        .parse(source)
    else {
        return Plan::Scan(REASON_NO_GRAM);
    };
    let mut folded = false;
    let clauses = facts(&hir, &mut folded).finish();
    if clauses.is_empty() {
        let plain = regex::escape(pattern) == pattern;
        return Plan::Scan(if plain && pattern.len() < 3 {
            REASON_SHORT
        } else {
            REASON_NO_GRAM
        });
    }
    Plan::Index {
        clauses,
        ignore_case: ignore_case || folded,
    }
}

/// What every line matched by a sub-pattern must contain.
///
/// `exact` is the complete set of strings the sub-pattern can match, when small.
/// `clauses` is an AND of OR-groups: each group names strings one of which occurs.
#[derive(Debug, Clone, Default)]
struct Facts {
    exact: Option<Vec<Vec<u8>>>,
    clauses: Vec<Vec<Vec<u8>>>,
}

impl Facts {
    /// No constraint.
    fn any() -> Self {
        Self::default()
    }

    fn exact(set: Vec<Vec<u8>>) -> Self {
        Self {
            exact: Some(set),
            clauses: Vec::new(),
        }
    }

    fn empty() -> Self {
        Self::exact(vec![Vec::new()])
    }

    /// A sub-pattern that occurs at least once, a varying number of times.
    fn required(sub: Self) -> Self {
        Self {
            exact: None,
            clauses: sub.finish(),
        }
    }

    /// Concatenation unions the constraints of both sides.
    fn concat(self, next: Self) -> Self {
        let mut clauses = self.clauses;
        clauses.extend(next.clauses);
        match (self.exact, next.exact) {
            (Some(left), Some(right)) if left.len() * right.len() <= EXACT_CAP => Self {
                exact: Some(product(&left, &right)),
                clauses,
            },
            (left, right) => {
                clauses.extend(clause(left.as_deref()));
                clauses.extend(clause(right.as_deref()));
                Self {
                    exact: None,
                    clauses,
                }
            }
        }
    }

    /// Alternation intersects: only a group that each branch implies survives.
    fn alternate(self, other: Self) -> Self {
        if let (Some(left), Some(right)) = (&self.exact, &other.exact)
            && left.len() + right.len() <= EXACT_CAP
        {
            let set: BTreeSet<Vec<u8>> = left.iter().chain(right).cloned().collect();
            return Self::exact(set.into_iter().collect());
        }
        match (self.best(), other.best()) {
            (Some(left), Some(right)) => {
                let set: BTreeSet<Vec<u8>> = left.into_iter().chain(right).collect();
                Self {
                    exact: None,
                    clauses: vec![set.into_iter().collect()],
                }
            }
            _ => Self::any(),
        }
    }

    /// The most selective single group: fewest strings, then the longest shortest string.
    fn best(self) -> Option<Vec<Vec<u8>>> {
        self.finish().into_iter().min_by_key(|group| {
            let shortest = group.iter().map(Vec::len).min().unwrap_or(0);
            (group.len(), std::cmp::Reverse(shortest))
        })
    }

    /// Every group implied by these facts.
    fn finish(self) -> Vec<Vec<Vec<u8>>> {
        let mut clauses = self.clauses;
        clauses.extend(clause(self.exact.as_deref()));
        clauses
    }
}

/// An exact set becomes a group only when every string can carry a trigram.
fn clause(exact: Option<&[Vec<u8>]>) -> Option<Vec<Vec<u8>>> {
    let set = exact?;
    (!set.is_empty() && set.iter().all(|text| text.len() >= 3)).then(|| set.to_vec())
}

fn product(left: &[Vec<u8>], right: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let set: BTreeSet<Vec<u8>> = left
        .iter()
        .flat_map(|head| {
            right
                .iter()
                .map(move |tail| [head.as_slice(), tail.as_slice()].concat())
        })
        .collect();
    set.into_iter().collect()
}

fn facts(hir: &Hir, folded: &mut bool) -> Facts {
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => Facts::empty(),
        HirKind::Literal(literal) => Facts::exact(vec![literal.0.to_vec()]),
        HirKind::Class(class) => class_facts(class, folded),
        HirKind::Repetition(repetition) => {
            if repetition.min == 0 {
                return Facts::any();
            }
            let sub = facts(&repetition.sub, folded);
            if repetition.min == 1 && repetition.max == Some(1) {
                sub
            } else {
                Facts::required(sub)
            }
        }
        HirKind::Capture(capture) => facts(&capture.sub, folded),
        HirKind::Concat(subs) => concat_facts(subs, folded),
        HirKind::Alternation(subs) => subs
            .iter()
            .map(|sub| facts(sub, folded))
            .reduce(Facts::alternate)
            .unwrap_or_else(Facts::any),
    }
}

/// Concatenation keeps runs of exact pieces together so trigrams across piece
/// boundaries survive an inexact neighbor.
fn concat_facts(subs: &[Hir], folded: &mut bool) -> Facts {
    let mut out = Facts::empty();
    let mut run: Vec<Vec<u8>> = vec![Vec::new()];
    for sub in subs {
        let piece = facts(sub, folded);
        out.clauses.extend(piece.clauses);
        match piece.exact {
            Some(set) if run.len() * set.len() <= EXACT_CAP => run = product(&run, &set),
            Some(set) => out = out.concat(Facts::exact(std::mem::replace(&mut run, set))),
            None => {
                out = out.concat(Facts::exact(std::mem::replace(&mut run, vec![Vec::new()])));
                out = out.concat(Facts::any());
            }
        }
    }
    out.concat(Facts::exact(run))
}

/// A small class expands into its members. ASCII letters fold to lower case,
/// which asks the index for its case-insensitive postings.
fn class_facts(class: &Class, folded: &mut bool) -> Facts {
    let mut set = BTreeSet::new();
    match class {
        Class::Unicode(unicode) => {
            let size: u32 = unicode
                .ranges()
                .iter()
                .map(|range| u32::from(range.end()) - u32::from(range.start()) + 1)
                .sum();
            if size > CLASS_CAP {
                return Facts::any();
            }
            for member in unicode
                .ranges()
                .iter()
                .flat_map(|range| range.start()..=range.end())
            {
                let lower = member.to_ascii_lowercase();
                *folded |= lower != member;
                set.insert(lower.to_string().into_bytes());
            }
        }
        Class::Bytes(bytes) => {
            let size: u32 = bytes
                .ranges()
                .iter()
                .map(|range| u32::from(range.end()) - u32::from(range.start()) + 1)
                .sum();
            if size > CLASS_CAP {
                return Facts::any();
            }
            for member in bytes
                .ranges()
                .iter()
                .flat_map(|range| range.start()..=range.end())
            {
                let lower = member.to_ascii_lowercase();
                *folded |= lower != member;
                set.insert(vec![lower]);
            }
        }
    }
    Facts::exact(set.into_iter().collect())
}

/// Runs grep mode for one call. Seen rows and the page draft are returned,
/// not recorded: the caller records them only for the served result, after
/// the freshness check.
pub(crate) async fn run(
    search: &Search,
    args: &SearchArgs,
    scope: &Scope,
    call: &Call<'_>,
) -> Result<(String, Vec<SeenRow>, GrepDraft), SearchError> {
    let Query { matcher, plan } = compile(&args.pattern, args.literal, args.ignore_case)?;
    let glob = args.glob.as_deref().map(FindGlob::new).transpose()?;
    let universe = universe(scope, glob).await?;
    let (files, reason) = narrow(search, &plan, scope, call.workspace, universe.files).await;
    let keep_all = search.reranks(call);
    let limit = args.limit;
    let scanned = tokio::task::spawn_blocking(move || scan(files, &matcher, keep_all, limit))
        .await
        .map_err(|error| SearchError::msg(format!("search: {error}")))??;
    let mut order: Vec<usize> = (0..scanned.hits.len()).collect();
    let mut rerank_note = None;
    if keep_all {
        let candidates = scanned.candidates();
        let (permutation, note) = search
            .rerank(call, &args.pattern, "grep", &candidates)
            .await;
        if let Some(permutation) = permutation {
            order = permutation;
        }
        rerank_note = note;
    }
    let total = scanned.total;
    order.truncate(limit);
    let shown = order.len();
    let truncated = total > shown;
    let (body, seen, draft) = tokio::task::spawn_blocking(move || {
        let (body, seen) = render(&scanned, &order);
        (body, seen, draft(scanned, &order, truncated))
    })
    .await
    .map_err(|error| SearchError::msg(format!("search: {error}")))?;
    let mut text = if body.is_empty() {
        format!("No matches for {}", args.pattern)
    } else {
        body
    };
    if truncated {
        push_line(
            &mut text,
            Some(&format!("[Truncated: {} more matches]", total - shown)),
        );
    }
    if let Some(reason) = reason {
        push_line(&mut text, Some(&format!("[index: full scan ({reason})]")));
    }
    if universe.utf16 > 0 {
        push_line(
            &mut text,
            Some(&format!(
                "[Skipped {} files: UTF-16 without a BOM.]",
                universe.utf16
            )),
        );
    }
    push_line(&mut text, rerank_note.as_deref());
    Ok((text, seen, draft))
}

/// Narrows the universe through the index, or names why the whole universe is scanned.
async fn narrow(
    search: &Search,
    plan: &Plan,
    scope: &Scope,
    workspace: &Path,
    mut files: Vec<Target>,
) -> (Vec<Target>, Option<&'static str>) {
    let (clauses, ignore_case) = match plan {
        Plan::Scan(reason) => return (files, Some(*reason)),
        Plan::Index {
            clauses,
            ignore_case,
        } => (clauses, *ignore_case),
    };
    if scope.is_file {
        return (files, Some(REASON_SINGLE_FILE));
    }
    let Some(index_scope) = scope.index_scope() else {
        return (files, Some(REASON_NO_INDEX));
    };
    match search
        .index
        .search_candidates(workspace, clauses, ignore_case, index_scope)
        .await
    {
        Ok(Some(candidates)) => {
            let candidates: HashSet<PathBuf> = candidates.into_iter().collect();
            files.retain(|file| {
                file.ws_rel
                    .as_ref()
                    .is_some_and(|path| candidates.contains(path))
            });
            (files, None)
        }
        Ok(None) | Err(_) => (files, Some(REASON_NO_INDEX)),
    }
}

/// The current bytes of a file that had matches, with its line starts.
struct Text {
    bytes: Vec<u8>,
    starts: Vec<usize>,
}

impl Text {
    fn new(bytes: Vec<u8>) -> Self {
        let mut starts = Vec::new();
        if !bytes.is_empty() {
            starts.push(0);
        }
        starts.extend(
            bytes
                .iter()
                .enumerate()
                .filter(|&(at, &byte)| byte == b'\n' && at + 1 < bytes.len())
                .map(|(at, _)| at + 1),
        );
        Self { bytes, starts }
    }

    fn line_count(&self) -> u64 {
        u64::try_from(self.starts.len()).unwrap_or(u64::MAX)
    }

    /// Line `number` (1-based) without its line terminator.
    fn line(&self, number: u64) -> &[u8] {
        let Some(index) = usize::try_from(number).ok().and_then(|n| n.checked_sub(1)) else {
            return &[];
        };
        let Some(&start) = self.starts.get(index) else {
            return &[];
        };
        let end = self
            .starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.bytes.len());
        let line = &self.bytes[start..end];
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        line.strip_suffix(b"\r").unwrap_or(line)
    }
}

/// One verified file: its target and, when it may be shown, its bytes.
struct Scanned {
    files: Vec<(Target, Option<Text>)>,
    hits: Vec<Hit>,
    /// Every matched line, including hits discarded past `limit` when reranking is off.
    total: usize,
}

/// One matching line, identified by file index and 1-based line number.
struct Hit {
    file: usize,
    line: u64,
}

impl Scanned {
    fn candidates(&self) -> Vec<RerankCandidate> {
        self.hits
            .iter()
            .map(|hit| {
                let (target, text) = &self.files[hit.file];
                let line = text.as_ref().map_or(&[][..], |text| text.line(hit.line));
                RerankCandidate {
                    path: target.shown.as_str().into(),
                    display: row(&target.shown, hit.line, ':', line).into(),
                }
            })
            .collect()
    }
}

/// Collects matching line numbers; the searcher reports one call per matching line.
/// Only the first `cap` lines are kept; `total` still counts every match so
/// truncation footers stay exact while one pathological file cannot exhaust memory.
struct Lines {
    kept: Vec<u64>,
    cap: usize,
    total: usize,
}

impl Sink for Lines {
    type Error = io::Error;

    fn matched(&mut self, _searcher: &Searcher, found: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if let Some(line) = found.line_number() {
            self.total += 1;
            if self.kept.len() < self.cap {
                self.kept.push(line);
            }
        }
        Ok(true)
    }
}

/// Verifies every candidate against the bytes read now, in path order.
fn scan(
    files: Vec<Target>,
    matcher: &RegexMatcher,
    keep_all: bool,
    limit: usize,
) -> Result<Scanned, SearchError> {
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::none())
        .build();
    let mut out = Scanned {
        files: Vec::new(),
        hits: Vec::new(),
        total: 0,
    };
    for target in files {
        let fits = std::fs::metadata(&target.abs)
            .is_ok_and(|meta| meta.len() <= find::MAX_INDEXABLE_BYTES);
        if !fits {
            continue;
        }
        let Ok(bytes) = std::fs::read(&target.abs) else {
            continue;
        };
        let fits = u64::try_from(bytes.len()).is_ok_and(|len| len <= find::MAX_INDEXABLE_BYTES);
        if !fits || find::classify_probe(&bytes) != ContentClass::Text {
            continue;
        }
        let cap = if keep_all {
            RERANK_LINES_CAP
        } else {
            limit.saturating_sub(out.hits.len())
        };
        let mut lines = Lines {
            kept: Vec::new(),
            cap,
            total: 0,
        };
        searcher
            .search_slice(matcher, &bytes, &mut lines)
            .map_err(|error| SearchError::msg(format!("search: {}: {error}", target.shown)))?;
        if lines.total == 0 {
            continue;
        }
        out.total += lines.total;
        let keep = keep_all || !lines.kept.is_empty();
        let file = out.files.len();
        out.hits
            .extend(lines.kept.into_iter().map(|line| Hit { file, line }));
        out.files.push((target, keep.then(|| Text::new(bytes))));
    }
    Ok(out)
}

/// One `Seen.show` call: a contiguous displayed interval of one file.
pub(crate) struct SeenRow {
    pub(crate) key: String,
    pub(crate) digest: [u8; 32],
    pub(crate) first: u64,
    pub(crate) last: u64,
}

/// The typed page of the displayed hits over the buffers they were verified in.
fn draft(scanned: Scanned, order: &[usize], truncated: bool) -> GrepDraft {
    let Scanned {
        mut files, hits, ..
    } = scanned;
    let mut slots: Vec<Option<usize>> = vec![None; files.len()];
    let mut taken: Vec<(String, bool, Text)> = Vec::new();
    let mut drafted = Vec::new();
    for hit in order.iter().filter_map(|&index| hits.get(index)) {
        let slot = if let Some(slot) = slots[hit.file] {
            slot
        } else {
            let (target, text) = &mut files[hit.file];
            let Some(text) = text.take() else {
                continue;
            };
            taken.push((
                std::mem::take(&mut target.shown),
                target.ws_rel.is_some(),
                text,
            ));
            slots[hit.file] = Some(taken.len() - 1);
            taken.len() - 1
        };
        let raw = taken[slot].2.line(hit.line);
        let complete = !cropped(raw);
        let shown = crop(raw);
        let row_text = if complete {
            String::from_utf8_lossy(raw).into()
        } else {
            shown.as_str().into()
        };
        drafted.push(DraftHit {
            file: slot,
            text: shown,
            row: dal_core::SourceRow {
                line: hit.line,
                text: row_text,
                complete,
            },
        });
    }
    GrepDraft {
        files: taken
            .into_iter()
            .map(|(shown, in_workspace, text)| DraftFile {
                in_workspace,
                shown,
                lines: text.line_count(),
                bytes: text.bytes,
            })
            .collect(),
        hits: drafted,
        truncated,
    }
}

/// Formats the displayed hits grouped by file, with automatic context.
fn render(scanned: &Scanned, order: &[usize]) -> (String, Vec<SeenRow>) {
    let context: u64 = match order.len() {
        1 => 50,
        2 | 3 => 15,
        _ => 0,
    };
    let mut groups: Vec<(usize, BTreeSet<u64>)> = Vec::new();
    for hit in order.iter().filter_map(|&index| scanned.hits.get(index)) {
        match groups.iter_mut().find(|(file, _)| *file == hit.file) {
            Some((_, lines)) => {
                lines.insert(hit.line);
            }
            None => groups.push((hit.file, BTreeSet::from([hit.line]))),
        }
    }
    let mut rows = Vec::new();
    let mut seen = Vec::new();
    for (file, matched) in groups {
        let (target, Some(text)) = &scanned.files[file] else {
            continue;
        };
        let count = text.line_count();
        let shown: BTreeSet<u64> = matched
            .iter()
            .flat_map(|&line| {
                line.saturating_sub(context).max(1)..=line.saturating_add(context).min(count)
            })
            .collect();
        for &line in &shown {
            let marker = if matched.contains(&line) { ':' } else { '-' };
            rows.push(row(&target.shown, line, marker, text.line(line)));
        }
        let key = seen_key(&target.abs);
        let digest = digest32(&text.bytes);
        let whole: BTreeSet<u64> = shown
            .iter()
            .copied()
            .filter(|&line| !cropped(text.line(line)))
            .collect();
        for (first, last) in intervals(&whole) {
            seen.push(SeenRow {
                key: key.clone(),
                digest,
                first,
                last,
            });
        }
    }
    (rows.join("\n"), seen)
}

/// Contiguous runs of an ordered line set.
fn intervals(lines: &BTreeSet<u64>) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    for &line in lines {
        match out.last_mut() {
            Some((_, last)) if *last + 1 == line => *last = line,
            _ => out.push((line, line)),
        }
    }
    out
}

/// A match row `path:line: text` or a context row `path:line- text`.
fn row(path: &str, line: u64, marker: char, raw: &[u8]) -> String {
    format!("{path}:{line}{marker} {}", crop(raw))
}

/// Whether `crop` hides part of this line, so the row does not prove the whole line was seen.
fn cropped(raw: &[u8]) -> bool {
    String::from_utf8_lossy(raw)
        .chars()
        .nth(CROP_CHARS)
        .is_some()
}

/// Decodes one line lossily and cuts it at 500 characters plus `...`.
fn crop(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    match text.char_indices().nth(CROP_CHARS) {
        Some((at, _)) => format!("{}...", &text[..at]),
        None => text.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{engine, run as search, write};
    use super::*;

    fn index_plan(pattern: &str, ignore_case: bool) -> Plan {
        plan(pattern, pattern, false, ignore_case)
    }

    fn group(items: &[&str]) -> Vec<Vec<u8>> {
        items.iter().map(|item| item.as_bytes().to_vec()).collect()
    }

    #[test]
    fn plan_intersects_alternation_and_unions_concatenation() {
        assert_eq!(
            index_plan("foo|barn", false),
            Plan::Index {
                clauses: vec![group(&["barn", "foo"])],
                ignore_case: false
            }
        );
        assert_eq!(
            index_plan("alpha.*omega", false),
            Plan::Index {
                clauses: vec![group(&["alpha"]), group(&["omega"])],
                ignore_case: false
            }
        );
        assert_eq!(
            index_plan(r"\w+tail(x|y)z", false),
            Plan::Index {
                clauses: vec![group(&["tailxz", "tailyz"])],
                ignore_case: false
            }
        );
        assert_eq!(index_plan("foo|b", false), Plan::Scan(REASON_NO_GRAM));
        assert_eq!(index_plan("a.b", false), Plan::Scan(REASON_NO_GRAM));
        assert_eq!(index_plan("ab", false), Plan::Scan(REASON_SHORT));
        assert_eq!(plan("ab", "ab", true, false), Plan::Scan(REASON_SHORT));
        assert_eq!(index_plan("né", true), Plan::Scan(REASON_NON_ASCII));
    }

    #[test]
    fn case_insensitive_plan_keeps_unicode_folds_of_ascii_letters() {
        let Plan::Index {
            clauses,
            ignore_case,
        } = index_plan("key", true)
        else {
            panic!("expected an index plan");
        };
        assert!(ignore_case);
        assert_eq!(clauses, vec![group(&["key", "\u{212A}ey"])]);
    }

    #[tokio::test]
    async fn short_literal_triggers_full_scan() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", b"xx ab yy\nnone\n");
        write(dir.path(), "b.txt", b"ab\n");
        let index = tempfile::tempdir().unwrap();
        let search_engine = engine(false, Some(index.path().to_path_buf()));
        let text = search(
            &search_engine,
            dir.path(),
            r#"{"mode":"grep","pattern":"ab","literal":true}"#,
        )
        .await
        .unwrap();
        assert_eq!(
            text,
            "a.txt:1: xx ab yy\na.txt:2- none\nb.txt:1: ab\n[index: full scan (literal shorter than 3 bytes)]"
        );
    }

    #[tokio::test]
    async fn utf16_file_excluded_honestly() {
        let dir = tempfile::tempdir().unwrap();
        let utf16: Vec<u8> = "needle in utf16\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        write(dir.path(), "wide.txt", &utf16);
        let text = search(
            &engine(false, None),
            dir.path(),
            r#"{"mode":"grep","pattern":"needle"}"#,
        )
        .await
        .unwrap();
        assert_eq!(
            text,
            "No matches for needle\n[index: full scan (no index yet)]\n[Skipped 1 files: UTF-16 without a BOM.]"
        );
    }

    fn numbered(count: u64, hits: &[u64]) -> Vec<u8> {
        (1..=count)
            .map(|line| {
                if hits.contains(&line) {
                    format!("hit {line}\n")
                } else {
                    format!("line {line}\n")
                }
            })
            .collect::<String>()
            .into_bytes()
    }

    fn rows(text: &str, marker: char) -> usize {
        text.lines()
            .filter_map(|line| line.strip_prefix("f.txt:"))
            .filter(|rest| {
                rest.trim_start_matches(|c: char| c.is_ascii_digit())
                    .starts_with(marker)
            })
            .count()
    }

    #[tokio::test]
    async fn grep_auto_context() {
        let cases: [(&[u64], usize); 3] =
            [(&[100], 100), (&[50, 150], 60), (&[20, 60, 100, 140], 0)];
        for (hits, context) in cases {
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), "f.txt", &numbered(200, hits));
            let text = search(
                &engine(false, None),
                dir.path(),
                r#"{"mode":"grep","pattern":"hit \\d+"}"#,
            )
            .await
            .unwrap();
            assert_eq!(rows(&text, ':'), hits.len(), "{text}");
            assert_eq!(rows(&text, '-'), context, "{text}");
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", &numbered(10, &[1, 5]));
        let text = search(
            &engine(false, None),
            dir.path(),
            r#"{"mode":"grep","pattern":"hit","limit":1}"#,
        )
        .await
        .unwrap();
        let expected: Vec<String> = std::iter::once("f.txt:1: hit 1".to_owned())
            .chain((2..=10).map(|line| {
                let body = if line == 5 {
                    "hit 5".to_owned()
                } else {
                    format!("line {line}")
                };
                format!("f.txt:{line}- {body}")
            }))
            .chain([
                "[Truncated: 1 more matches]".to_owned(),
                "[index: full scan (no index yet)]".to_owned(),
            ])
            .collect();
        assert_eq!(text, expected.join("\n"));
    }

    #[tokio::test]
    async fn long_lines_are_cut_at_500_characters() {
        let dir = tempfile::tempdir().unwrap();
        let line = format!("{}needle", "é".repeat(600));
        write(dir.path(), "f.txt", format!("{line}\n").as_bytes());
        let text = search(
            &engine(false, None),
            dir.path(),
            r#"{"mode":"grep","pattern":"needle"}"#,
        )
        .await
        .unwrap();
        assert_eq!(
            text.lines().next().unwrap(),
            format!("f.txt:1: {}...", "é".repeat(500))
        );
    }
    #[tokio::test]
    async fn limit_applies_across_files_with_exact_truncation() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", &numbered(5, &[1]));
        write(dir.path(), "b.txt", &numbered(5, &[1, 2]));
        let text = search(
            &engine(false, None),
            dir.path(),
            r#"{"mode":"grep","pattern":"hit","limit":2}"#,
        )
        .await
        .unwrap();
        assert_eq!(
            text.lines().filter(|line| line.contains(": hit")).count(),
            2,
            "{text}"
        );
        assert!(text.contains("[Truncated: 1 more matches]"), "{text}");
    }

    #[tokio::test]
    async fn nested_gitignore_recall() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".gitignore", b"*.log\n");
        write(dir.path(), "sub/.gitignore", b"!keep.log\n");
        write(dir.path(), "sub/keep.log", b"rare_token_zq\n");
        write(dir.path(), "drop.log", b"rare_token_zq\n");
        for index_root in [None, Some(tempfile::tempdir().unwrap())] {
            let root = index_root.as_ref().map(|dir| dir.path().to_path_buf());
            let text = search(
                &engine(false, root),
                dir.path(),
                r#"{"mode":"grep","pattern":"rare_token_zq"}"#,
            )
            .await
            .unwrap();
            assert_eq!(
                text.lines().next(),
                Some("sub/keep.log:1: rare_token_zq"),
                "{text}"
            );
            assert!(!text.contains("drop.log"), "{text}");
        }
    }

    /// Match rows `(path, line)` of a result, ignoring context and notes.
    fn identities(text: &str) -> Vec<(String, u64)> {
        text.lines()
            .filter_map(|row| {
                let (head, _) = row.split_once(": ")?;
                let (path, line) = head.rsplit_once(':')?;
                Some((path.to_owned(), line.parse().ok()?))
            })
            .collect()
    }

    /// An independent oracle: every text file of the walk, each line tested with `regex`.
    fn oracle(root: &Path, pattern: &str) -> Vec<(String, u64)> {
        let regex = regex::bytes::Regex::new(pattern).unwrap();
        let mut out = Vec::new();
        for entry in find::walk(root)
            .unwrap()
            .into_iter()
            .filter(|entry| !entry.is_dir && entry.indexable)
        {
            let bytes = std::fs::read(root.join(&entry.path)).unwrap();
            let mut lines: Vec<&[u8]> = bytes.split(|&byte| byte == b'\n').collect();
            if lines.last().is_some_and(|line| line.is_empty()) {
                lines.pop();
            }
            for (at, line) in lines.into_iter().enumerate() {
                if regex.is_match(line) {
                    out.push((find::display(&entry.path), u64::try_from(at + 1).unwrap()));
                }
            }
        }
        out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()).then(a.1.cmp(&b.1)));
        out
    }

    #[tokio::test]
    async fn grep_conformance() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".gitignore", b"ignored/\n");
        write(dir.path(), "ignored/x.txt", b"alpha beta\n");
        write(
            dir.path(),
            "src/a.rs",
            b"fn alpha() {}\nlet beta = 1;\nalphabet soup\n",
        );
        write(dir.path(), "src/b.rs", b"gamma\nALPHA\nbeta alpha\n");
        write(dir.path(), "c.md", b"alpha\n\nbeta\ngamma delta\n");
        write(dir.path(), "bin.dat", b"alpha\0beta\n");
        let patterns = [
            "alpha",
            "beta|gamma",
            r"alph\w+",
            "a.p",
            "(?i)alpha",
            r"^\w+$",
            "delta",
        ];
        let index = tempfile::tempdir().unwrap();
        for index_root in [None, Some(index.path().to_path_buf())] {
            let engine = engine(false, index_root);
            for pattern in patterns {
                let raw = format!(
                    r#"{{"mode":"grep","pattern":{},"limit":1000}}"#,
                    sonic_rs::to_string(pattern).unwrap()
                );
                let text = search(&engine, dir.path(), &raw).await.unwrap();
                assert_eq!(
                    identities(&text),
                    oracle(dir.path(), pattern),
                    "{pattern}: {text}"
                );
            }
        }
    }

    #[tokio::test]
    async fn changed_file_is_verified_against_current_bytes() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", b"wombat_alpha\n");
        let index = tempfile::tempdir().unwrap();
        let engine = engine(false, Some(index.path().to_path_buf()));
        let first = search(
            &engine,
            dir.path(),
            r#"{"mode":"grep","pattern":"wombat_alpha"}"#,
        )
        .await
        .unwrap();
        assert_eq!(first.lines().next(), Some("a.txt:1: wombat_alpha"));
        write(dir.path(), "a.txt", b"wombat_gamma\n");
        engine.index.dirty(dir.path(), &dir.path().join("a.txt"));
        let second = search(
            &engine,
            dir.path(),
            r#"{"mode":"grep","pattern":"wombat_alpha"}"#,
        )
        .await
        .unwrap();
        assert!(
            second.starts_with("No matches for wombat_alpha"),
            "{second}"
        );
    }
}
