//! The incremental DFA engine: bounded condition compilation and the
//! byte-wise stream feed.
//!
//! Each kept condition compiles to one dense DFA (regex-automata 0.4.18).
//! A [`StreamState`] holds one persistent DFA state per admitted condition
//! of one stream source and a 256-byte ring for the fire excerpt; it never
//! keeps the stream text. Feeding costs O(delta bytes) per live condition.
//!
//! Match semantics per delta: every byte advances the DFA and
//! `is_match_state` is checked after it, because the DFA reports each match
//! one byte late; after the last byte, the end-of-text query
//! `next_eoi_state` evaluates `$`, `\z`, and `\b` as if the text ended at the
//! delta boundary. A dead or quit state latches the condition off; a fire
//! latches every condition of its rule, so one rule fires at most once per
//! stream state.

use std::fmt;
use std::sync::Arc;

use regex_automata::dfa::{Automaton, StartKind, dense};
use regex_automata::nfa::thompson;
use regex_automata::util::primitives::StateID;
use regex_automata::util::{start, syntax};
use regex_automata::{Anchored, MatchKind};

use super::value::{CONDITION_MAX_BYTES, ConditionSource, Origin, Problem, ProblemKind, Severity};

/// Most NFA states one condition may expand to.
pub const NODE_LIMIT: usize = 10_000;

/// Byte budget of both the determinization and the finished DFA of one
/// condition.
pub const DFA_BUDGET_BYTES: usize = 4_194_304;

/// Bytes of stream tail kept per stream state for the excerpt.
pub const RING_BYTES: usize = 256;

/// Longest excerpt in bytes, before the cut to the first UTF-8 lead byte.
pub const EXCERPT_BYTES: usize = 240;

/// Memory backstop of the NFA build so a huge counted repetition stops
/// early; a build that hits it is reported as the node limit.
const NFA_BYTES_BACKSTOP: usize = NODE_LIMIT * 512;

/// Why one condition was skipped at compile time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SkipReason {
    /// The source is longer than [`CONDITION_MAX_BYTES`].
    TooLong,
    /// A leading inline flag group holds letters other than `i`, `m`, `s`;
    /// the payload is the group as written, for example `(?x)`.
    FlagGroup(Box<str>),
    /// An unescaped `[...]` class holds a non-ASCII byte.
    NonAsciiClass,
    /// The pattern expands past [`NODE_LIMIT`] NFA states.
    NodeLimit,
    /// Determinization or the DFA exceeds [`DFA_BUDGET_BYTES`].
    Budget,
    /// The regex does not compile in the dal dialect.
    Syntax,
}

/// A condition skipped at compile time; `Display` is the exact remark.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Skip {
    /// Zero-based position in the rule's condition list.
    pub index: usize,
    /// The condition source as written.
    pub src: Box<str>,
    /// Why it was skipped.
    pub reason: SkipReason,
}

impl Skip {
    /// Returns the Skipped condition remark of the problem table.
    #[must_use]
    pub fn problem(&self, origin: Origin) -> Problem {
        Problem {
            origin,
            kind: ProblemKind::Condition,
            reason: self.to_string(),
            consequence: String::new(),
            severity: Severity::Skipped,
        }
    }
}

impl fmt::Display for Skip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (n, src) = (self.index + 1, &self.src);
        match &self.reason {
            SkipReason::TooLong => write!(f, "condition {n} is longer than 1024 bytes"),
            SkipReason::FlagGroup(group) => write!(
                f,
                "condition {n} \"{src}\" starts with the inline flag group \"{group}\"; dalgon accepts only i, m, and s there"
            ),
            SkipReason::NonAsciiClass => write!(
                f,
                "condition {n} \"{src}\" has a non-ASCII character inside [...]; dal matches bytes, so write such characters as alternatives, for example (é|è)"
            ),
            SkipReason::NodeLimit => write!(
                f,
                "condition {n} \"{src}\" expands past the 10000-node pattern limit"
            ),
            SkipReason::Budget => write!(
                f,
                "condition {n} \"{src}\" expands past the 4194304-byte matcher budget"
            ),
            SkipReason::Syntax => write!(
                f,
                "condition {n} \"{src}\" does not compile: dalgon reads Rust regex syntax without lookaround, backreferences, or inline flags after the start"
            ),
        }
    }
}

impl std::error::Error for Skip {}

/// One compiled condition: an immutable dense DFA and its start state.
#[derive(Debug)]
pub struct Compiled {
    dfa: dense::DFA<Vec<u32>>,
    start: StateID,
    index: usize,
    src: Box<str>,
    matches_empty: bool,
}

impl Compiled {
    /// Zero-based position in the rule's condition list.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    /// The condition source as written.
    #[must_use]
    pub fn src(&self) -> &str {
        &self.src
    }

    /// Whether the condition matches the empty text, so it fires on the
    /// first byte of every stream it watches.
    #[must_use]
    pub fn matches_empty(&self) -> bool {
        self.matches_empty
    }
}

/// Compiles one condition in the dal dialect under the pattern and DFA budgets.
///
/// The dialect is Rust regex syntax over bytes with Unicode classes off and
/// the ASCII `\b`; only a leading `(?i)`, `(?m)`, `(?s)` group (or their
/// combination) sets flags.
///
/// # Errors
///
/// Returns the first failing check in this order: length, leading flag
/// group, non-ASCII class byte, node limit, DFA budget, compile.
pub fn compile(src: &str, index: usize) -> Result<Compiled, Skip> {
    let fail = |reason: SkipReason| Skip {
        index,
        src: src.into(),
        reason,
    };
    if src.len() > CONDITION_MAX_BYTES {
        return Err(fail(SkipReason::TooLong));
    }
    let body = match leading_flag_group(src) {
        Some((letters, group)) => {
            if !letters.bytes().all(|b| matches!(b, b'i' | b'm' | b's')) {
                return Err(fail(SkipReason::FlagGroup(group.into())));
            }
            group.len()
        }
        None => 0,
    };
    let scan = scan(&src.as_bytes()[body..]);
    if scan.non_ascii_class {
        return Err(fail(SkipReason::NonAsciiClass));
    }
    if scan.inline_flags {
        return Err(fail(SkipReason::Syntax));
    }
    let nfa = bounded_nfa(src).map_err(fail)?;
    let dfa = dense::Builder::new()
        .configure(
            dense::Config::new()
                .match_kind(MatchKind::All)
                .start_kind(StartKind::Unanchored)
                .accelerate(false)
                .unicode_word_boundary(false)
                .dfa_size_limit(Some(DFA_BUDGET_BYTES))
                .determinize_size_limit(Some(DFA_BUDGET_BYTES)),
        )
        .build_from_nfa(&nfa)
        .map_err(|err| {
            fail(if err.is_size_limit_exceeded() {
                SkipReason::Budget
            } else {
                SkipReason::Syntax
            })
        })?;
    let start = dfa
        .start_state(&start::Config::new().anchored(Anchored::No))
        .map_err(|_| fail(SkipReason::Syntax))?;
    let matches_empty = dfa.is_match_state(dfa.next_eoi_state(start));
    Ok(Compiled {
        dfa,
        start,
        index,
        src: src.into(),
        matches_empty,
    })
}

/// Enforces the pattern-expansion budget before the more expensive determinization.
fn bounded_nfa(src: &str) -> Result<thompson::NFA, SkipReason> {
    let nfa = thompson::Compiler::new()
        .syntax(syntax::Config::new().unicode(false).utf8(false))
        .configure(
            thompson::Config::new()
                .utf8(false)
                .which_captures(thompson::WhichCaptures::None)
                .nfa_size_limit(Some(NFA_BYTES_BACKSTOP)),
        )
        .build(src)
        .map_err(|error| {
            if error.size_limit().is_some() {
                SkipReason::NodeLimit
            } else {
                SkipReason::Syntax
            }
        })?;
    if nfa.states().len() > NODE_LIMIT {
        return Err(SkipReason::NodeLimit);
    }
    Ok(nfa)
}

/// Returns the letters and the full text of a leading `(?letters)` or
/// `(?letters:` group; `(?:`, `(?P<`, `(?=` and the like are not flag groups.
fn leading_flag_group(src: &str) -> Option<(&str, &str)> {
    let rest = src.strip_prefix("(?")?;
    let end = flag_letters_len(rest.as_bytes());
    match rest.as_bytes().get(end) {
        Some(b')' | b':') if end > 0 => Some((&rest[..end], &src[..end + 3])),
        _ => None,
    }
}

fn flag_letters_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|b| b.is_ascii_alphabetic() || **b == b'-')
        .count()
}

/// What the source scan found after the leading flag group.
#[derive(Default)]
struct Scan {
    non_ascii_class: bool,
    inline_flags: bool,
}

/// Scans a pattern body once: a non-ASCII byte inside an unescaped class,
/// and an inline flag group outside classes.
fn scan(bytes: &[u8]) -> Scan {
    let mut out = Scan::default();
    let mut depth = 0_usize;
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        i += 1;
        match b {
            b'\\' => {
                if depth > 0 && bytes.get(i).is_some_and(|&n| n >= 0x80) {
                    out.non_ascii_class = true;
                }
                i = skip_char(bytes, i);
            }
            b'[' => {
                depth += 1;
                i = skip_class_open(bytes, i);
            }
            b']' if depth > 0 => depth -= 1,
            b'(' if depth == 0 => {
                if let Some(rest) = bytes[i..].strip_prefix(b"?") {
                    let end = flag_letters_len(rest);
                    if end > 0 && matches!(rest.get(end), Some(b')' | b':')) {
                        out.inline_flags = true;
                    }
                }
            }
            0x80.. if depth > 0 => out.non_ascii_class = true,
            _ => {}
        }
    }
    out
}

/// Skips the escaped character that starts at `i`, including its UTF-8
/// continuation bytes.
fn skip_char(bytes: &[u8], mut i: usize) -> usize {
    if i < bytes.len() {
        i += 1;
        while bytes.get(i).is_some_and(|&b| is_continuation(b)) {
            i += 1;
        }
    }
    i
}

/// Skips the `^` and the literal `]` that may open a class.
fn skip_class_open(bytes: &[u8], mut i: usize) -> usize {
    if bytes.get(i) == Some(&b'^') {
        i += 1;
    }
    if bytes.get(i) == Some(&b']') {
        i += 1;
    }
    i
}

/// Reports whether `src` is glob shorthand: none of
/// `\ ^ $ + | ( )`, at least one of `? * [ ] { }`, and either a `/` or the
/// shape `*.<ext>` with no whitespace.
#[must_use]
pub fn is_glob_shorthand(src: &str) -> bool {
    !src.contains(['\\', '^', '$', '+', '|', '(', ')'])
        && src.contains(['?', '*', '[', ']', '{', '}'])
        && (src.contains('/')
            || src
                .strip_prefix("*.")
                .is_some_and(|ext| !ext.is_empty() && !ext.contains(char::is_whitespace)))
}

/// A rule's conditions after the glob shorthand step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shorthand {
    /// The regex conditions kept, with their original indexes; `[".*"]` at
    /// index 0 when every condition was shorthand.
    pub conditions: Vec<ConditionSource>,
    /// One `tool:patch(<glob>)` scope token per shorthand condition, in order.
    pub tokens: Vec<Box<str>>,
    /// Whether `.*` was inserted because no regex condition was left.
    pub fallback: bool,
}

/// Splits glob shorthand conditions into scope tokens. An input with no
/// condition stays empty; an input whose every condition is shorthand gets
/// the condition list `[".*"]`.
#[must_use]
pub fn split_shorthand(conditions: &[ConditionSource]) -> Shorthand {
    let mut kept = Vec::with_capacity(conditions.len());
    let mut tokens = Vec::new();
    for cond in conditions {
        if is_glob_shorthand(&cond.src) {
            tokens.push(format!("tool:patch({})", cond.src).into_boxed_str());
        } else {
            kept.push(cond.clone());
        }
    }
    let fallback = kept.is_empty() && !conditions.is_empty();
    if fallback {
        kept.push(ConditionSource {
            index: 0,
            src: ".*".into(),
        });
    }
    Shorthand {
        conditions: kept,
        tokens,
        fallback,
    }
}

/// Compiles the kept conditions of one rule, appending one Skipped remark
/// per failed condition and the empty-text note per kept condition that
/// matches the empty text, except the `.*` shorthand fallback.
pub fn compile_conditions(
    shorthand: &Shorthand,
    origin: &Origin,
    problems: &mut Vec<Problem>,
) -> Vec<Arc<Compiled>> {
    let mut kept = Vec::with_capacity(shorthand.conditions.len());
    for cond in &shorthand.conditions {
        match compile(&cond.src, cond.index) {
            Ok(compiled) => {
                if compiled.matches_empty && !shorthand.fallback {
                    problems.push(Problem {
                        origin: origin.clone(),
                        kind: ProblemKind::SetNote,
                        reason: format!(
                            "condition {} \"{}\" matches empty text, so it fires on the first output of every stream it watches.",
                            cond.index + 1,
                            cond.src
                        ),
                        consequence: String::new(),
                        severity: Severity::Note,
                    });
                }
                kept.push(Arc::new(compiled));
            }
            Err(skip) => problems.push(skip.problem(origin.clone())),
        }
    }
    kept
}

/// One rule fire found by [`StreamState::feed`].
#[derive(Clone, Debug)]
pub struct MatchFire {
    /// The caller's rule slot given to [`StreamState::add_rule`].
    pub rule: usize,
    /// The condition that matched.
    pub condition: Arc<Compiled>,
    /// The stream tail at the fire: at most 240 bytes, starting at a UTF-8
    /// lead byte, ending on a whole character.
    pub excerpt: String,
}

/// Matcher state of one stream source: one DFA state per admitted
/// condition and the excerpt ring.
#[derive(Debug, Default)]
pub struct StreamState {
    conds: Vec<CondState>,
    ring: Ring,
}

#[derive(Debug)]
struct CondState {
    rule: usize,
    compiled: Arc<Compiled>,
    sid: StateID,
    latched: bool,
}

/// Outcome of one condition over one delta.
enum Step {
    Live(StateID),
    Latch,
    Match,
}

impl StreamState {
    /// Returns an empty state that watches nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Admits the compiled conditions of one rule under the caller's slot
    /// `rule`; a fire reports that slot.
    pub fn add_rule(&mut self, rule: usize, conditions: &[Arc<Compiled>]) {
        self.conds
            .extend(conditions.iter().map(|compiled| CondState {
                rule,
                compiled: Arc::clone(compiled),
                sid: compiled.start,
                latched: false,
            }));
    }

    /// Reports whether every condition is latched, so no later delta can
    /// fire.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.conds.iter().all(|c| c.latched)
    }

    /// Feeds one delta and appends its fires in condition order. An empty
    /// delta is not fed and cannot fire. A fire latches every condition of
    /// its rule, so two conditions of one rule matching in one delta fire
    /// once.
    pub fn feed(&mut self, bytes: &[u8], fires: &mut Vec<MatchFire>) {
        if bytes.is_empty() {
            return;
        }
        self.ring.push(bytes);
        let base = fires.len();
        for cond in &mut self.conds {
            if cond.latched || fires[base..].iter().any(|f| f.rule == cond.rule) {
                continue;
            }
            match step(&cond.compiled.dfa, cond.sid, bytes) {
                Step::Live(sid) => cond.sid = sid,
                Step::Latch => cond.latched = true,
                Step::Match => fires.push(MatchFire {
                    rule: cond.rule,
                    condition: Arc::clone(&cond.compiled),
                    excerpt: self.ring.excerpt(),
                }),
            }
        }
        let new_matches = &fires[base..];
        if !new_matches.is_empty() {
            for cond in &mut self.conds {
                cond.latched |= new_matches.iter().any(|f| f.rule == cond.rule);
            }
        }
    }

    /// The current excerpt of the stream tail.
    #[must_use]
    pub fn excerpt(&self) -> String {
        self.ring.excerpt()
    }
}

/// Advances one DFA over one delta, then runs the end-of-text query.
fn step(dfa: &dense::DFA<Vec<u32>>, mut sid: StateID, bytes: &[u8]) -> Step {
    for &b in bytes {
        sid = dfa.next_state(sid, b);
        if dfa.is_special_state(sid) {
            if dfa.is_match_state(sid) {
                return Step::Match;
            }
            if dfa.is_dead_state(sid) || dfa.is_quit_state(sid) {
                return Step::Latch;
            }
        }
    }
    if dfa.is_match_state(dfa.next_eoi_state(sid)) {
        Step::Match
    } else {
        Step::Live(sid)
    }
}

/// The last [`RING_BYTES`] bytes of a stream.
#[derive(Debug)]
struct Ring {
    buf: [u8; RING_BYTES],
    end: usize,
    len: usize,
}

impl Default for Ring {
    fn default() -> Self {
        Self {
            buf: [0; RING_BYTES],
            end: 0,
            len: 0,
        }
    }
}

impl Ring {
    fn push(&mut self, bytes: &[u8]) {
        let bytes = &bytes[bytes.len().saturating_sub(RING_BYTES)..];
        let first = bytes.len().min(RING_BYTES - self.end);
        self.buf[self.end..self.end + first].copy_from_slice(&bytes[..first]);
        self.buf[..bytes.len() - first].copy_from_slice(&bytes[first..]);
        self.end = (self.end + bytes.len()) % RING_BYTES;
        self.len = (self.len + bytes.len()).min(RING_BYTES);
    }

    fn excerpt(&self) -> String {
        let take = self.len.min(EXCERPT_BYTES);
        let start = (self.end + RING_BYTES - take) % RING_BYTES;
        let first = take.min(RING_BYTES - start);
        let mut tail = [0_u8; EXCERPT_BYTES];
        tail[..first].copy_from_slice(&self.buf[start..start + first]);
        tail[first..take].copy_from_slice(&self.buf[..take - first]);
        utf8_excerpt(&tail[..take])
    }
}

/// Cuts `tail` to start at its first UTF-8 lead byte and to drop a
/// trailing incomplete character.
fn utf8_excerpt(tail: &[u8]) -> String {
    let lead = tail
        .iter()
        .position(|&b| !is_continuation(b))
        .unwrap_or(tail.len());
    let tail = &tail[lead..];
    let end = match tail.iter().rposition(|&b| !is_continuation(b)) {
        Some(last) if tail.len() - last < utf8_width(tail[last]) => last,
        _ => tail.len(),
    };
    String::from_utf8_lossy(&tail[..end]).into_owned()
}

fn is_continuation(b: u8) -> bool {
    b & 0xC0 == 0x80
}

fn utf8_width(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use proptest::prelude::*;

    use super::*;

    fn origin() -> Origin {
        Origin::User(PathBuf::from("/home/u/.dal/rules/a.md"))
    }

    fn cond(index: usize, src: &str) -> ConditionSource {
        ConditionSource {
            index,
            src: src.into(),
        }
    }

    fn state_for(rules: &[&[&str]]) -> StreamState {
        let mut state = StreamState::new();
        for (rule, srcs) in rules.iter().enumerate() {
            let compiled: Vec<_> = srcs
                .iter()
                .enumerate()
                .map(|(i, src)| Arc::new(compile(src, i).unwrap()))
                .collect();
            state.add_rule(rule, &compiled);
        }
        state
    }

    /// Returns the index of the delta whose feed fired the single rule.
    fn fire_delta(pattern: &str, deltas: &[&[u8]]) -> Option<usize> {
        let mut state = state_for(&[&[pattern]]);
        let mut fires = Vec::new();
        for (d, delta) in deltas.iter().enumerate() {
            state.feed(delta, &mut fires);
            if !fires.is_empty() {
                return Some(d);
            }
        }
        None
    }

    fn split<'a>(text: &'a [u8], cuts: &[usize]) -> Vec<&'a [u8]> {
        let mut points: Vec<usize> = cuts.iter().map(|c| c % (text.len() + 1)).collect();
        points.push(0);
        points.push(text.len());
        points.sort_unstable();
        points.dedup();
        points.windows(2).map(|w| &text[w[0]..w[1]]).collect()
    }

    fn delta_containing(deltas: &[&[u8]], byte: usize) -> usize {
        let mut end = 0;
        for (d, delta) in deltas.iter().enumerate() {
            end += delta.len();
            if byte < end {
                return d;
            }
        }
        deltas.len() - 1
    }

    fn oracle(pattern: &str) -> regex::bytes::Regex {
        regex::bytes::RegexBuilder::new(pattern)
            .unicode(false)
            .build()
            .unwrap()
    }

    const PIECES: &[&str] = &[
        "sleep",
        " ",
        "(",
        "git commit -m",
        "git ",
        "commit",
        "TODO",
        "TOD",
        "x",
        "é",
        "\n",
        "s",
        "foo",
        "_",
        "한",
    ];

    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(prop::sample::select(PIECES), 0..24).prop_map(|p| p.concat())
    }

    #[test]
    fn condition_dialect() {
        let mut state = state_for(&[&["(?i)todo"]]);
        let mut fires = Vec::new();
        state.feed(b"a ToDo", &mut fires);
        assert_eq!(fires.len(), 1);

        let conds = [
            cond(0, "(?i)todo"),
            cond(1, "(?x)a"),
            cond(2, "(?=a)b"),
            cond(3, "[é]"),
            cond(4, "(a{100}){101}"),
        ];
        let mut problems = Vec::new();
        let kept = compile_conditions(&split_shorthand(&conds), &origin(), &mut problems);
        assert_eq!(kept.len(), 1);
        let reasons: Vec<&str> = problems.iter().map(|p| p.reason.as_str()).collect();
        assert_eq!(
            reasons,
            [
                "condition 2 \"(?x)a\" starts with the inline flag group \"(?x)\"; dalgon accepts only i, m, and s there",
                "condition 3 \"(?=a)b\" does not compile: dalgon reads Rust regex syntax without lookaround, backreferences, or inline flags after the start",
                "condition 4 \"[é]\" has a non-ASCII character inside [...]; dal matches bytes, so write such characters as alternatives, for example (é|è)",
                "condition 5 \"(a{100}){101}\" expands past the 10000-node pattern limit",
            ]
        );
        assert!(
            problems
                .iter()
                .all(|p| p.severity == Severity::Skipped && p.kind == ProblemKind::Condition)
        );

        let shorthand = split_shorthand(&[cond(0, "*.rs"), cond(1, "src/**/*.ml")]);
        assert!(shorthand.fallback);
        let tokens: [Box<str>; 2] = ["tool:patch(*.rs)".into(), "tool:patch(src/**/*.ml)".into()];
        assert_eq!(shorthand.tokens, tokens);
        let mut problems = Vec::new();
        let kept = compile_conditions(&shorthand, &origin(), &mut problems);
        assert_eq!((kept.len(), problems.len()), (1, 0));

        let mut problems = Vec::new();
        compile_conditions(&split_shorthand(&[cond(0, "x*")]), &origin(), &mut problems);
        assert_eq!(
            problems[0].reason,
            "condition 1 \"x*\" matches empty text, so it fires on the first output of every stream it watches."
        );
        assert_eq!(problems[0].severity, Severity::Note);
    }

    #[test]
    fn inline_flags_after_start_and_class_edges() {
        assert_eq!(compile("a(?i)b", 0).unwrap_err().reason, SkipReason::Syntax);
        assert_eq!(
            compile("a(?s:.)", 0).unwrap_err().reason,
            SkipReason::Syntax
        );
        assert_eq!(compile("(?ims)a", 0).map(|c| c.index()), Ok(0));
        assert!(compile("(?:a)(?P<n>b)", 0).is_ok());
        assert_eq!(
            compile(r"[\é]", 0).unwrap_err().reason,
            SkipReason::NonAsciiClass
        );
        assert!(compile(r"[\]\-a]", 0).is_ok());
        assert_eq!(compile(r"\é", 0).unwrap_err().reason, SkipReason::Syntax);
        assert_eq!(
            compile(r"[\]é]", 0).unwrap_err().reason,
            SkipReason::NonAsciiClass
        );
        assert!(compile("é|è", 0).is_ok());
        assert_eq!(
            compile("[]é]", 0).unwrap_err().reason,
            SkipReason::NonAsciiClass
        );
        assert_eq!(
            compile("(?-i)a", 0).unwrap_err().reason,
            SkipReason::FlagGroup("(?-i)".into())
        );
    }

    #[test]
    fn shorthand_shapes() {
        for yes in ["*.rs", "src/*.ml", "a/b?c", "*.{ml,mli}"] {
            assert!(is_glob_shorthand(yes), "{yes}");
        }
        for no in [
            "*.", "*. rs", "a*b", "src/(a)*", "TODO", r"\*.rs", "src/a.ml",
        ] {
            assert!(!is_glob_shorthand(no), "{no}");
        }
        assert_eq!(
            split_shorthand(&[]),
            Shorthand {
                conditions: vec![],
                tokens: vec![],
                fallback: false
            }
        );
        let mixed = split_shorthand(&[cond(0, "*.rs"), cond(1, "TODO")]);
        assert_eq!(
            (mixed.conditions, mixed.fallback),
            (vec![cond(1, "TODO")], false)
        );
    }

    #[test]
    fn end_assertion_parity() {
        assert_eq!(fire_delta("foo$", &[b"foo", b"bar"]), Some(0));
    }

    #[test]
    fn latch_fires_once() {
        let mut state = state_for(&[&[".*"], &["^x"]]);
        let mut fires = Vec::new();
        state.feed(b"y", &mut fires);
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].rule, 0);
        assert!(state.is_settled());
        for _ in 0..100_000 {
            state.feed(b"x", &mut fires);
        }
        assert_eq!(fires.len(), 1);
    }

    #[test]
    fn two_conditions_in_one_chunk_fire_once() {
        let mut state = state_for(&[&["a", "b"], &["b"]]);
        let mut fires = Vec::new();
        state.feed(b"", &mut fires);
        assert!(fires.is_empty());
        state.feed(b"ab", &mut fires);
        let rules: Vec<usize> = fires.iter().map(|f| f.rule).collect();
        assert_eq!(rules, [0, 1]);
        assert_eq!(fires[0].condition.index(), 0);
        assert_eq!(fires[0].excerpt, "ab");
    }

    #[test]
    fn dead_latch_and_no_quit() {
        let mut state = state_for(&[&["^foo"]]);
        let mut fires = Vec::new();
        state.feed(b"b", &mut fires);
        assert!(state.is_settled());
        state.feed(b"foo", &mut fires);
        assert!(fires.is_empty());

        let mut state = state_for(&[&[r"\bx\b"]]);
        for delta in ["é", "x", "é"] {
            state.feed(delta.as_bytes(), &mut fires);
        }
        assert_eq!(fires.len(), 1);
    }

    #[test]
    fn excerpt_cut_to_lead_byte() {
        // 302 bytes: the last 240 start at byte 62, the last byte of a
        // three-byte character, so the excerpt starts at byte 63.
        let mut state = state_for(&[&["zz"]]);
        let mut fires = Vec::new();
        let text = format!("{}{}", "한".repeat(100), "zz");
        for chunk in text.as_bytes().chunks(7) {
            state.feed(chunk, &mut fires);
        }
        assert_eq!(fires.len(), 1);
        let excerpt = &fires[0].excerpt;
        assert!(text.ends_with(excerpt.as_str()));
        assert_eq!(excerpt.len(), 239);

        let mut state = StreamState::new();
        state.feed(&"aé".as_bytes()[..2], &mut fires);
        assert_eq!(state.excerpt(), "a");
    }

    #[test]
    fn size_limits() {
        assert!(
            compile(&"a".repeat(1024), 0)
                .map_or_else(|s| s.reason != SkipReason::TooLong, |_| true)
        );
        let long = compile(&"a".repeat(1025), 6).unwrap_err();
        assert_eq!(long.to_string(), "condition 7 is longer than 1024 bytes");
        let budget = compile("[01]*1[01]{20}", 0).unwrap_err();
        assert_eq!(
            budget.to_string(),
            "condition 1 \"[01]*1[01]{20}\" expands past the 4194304-byte matcher budget"
        );
    }

    #[test]
    fn stress_engine_contract() {
        let mut state = state_for(&[&["(a|aa)*b"]]);
        let mut fires = Vec::new();
        let text = vec![b'a'; 1 << 20];
        let begun = Instant::now();
        for chunk in text.chunks(64) {
            state.feed(chunk, &mut fires);
        }
        assert!(begun.elapsed() < Duration::from_millis(100));
        assert!(fires.is_empty());

        // Contract pin of regex-automata 0.4.18 facts the feed relies on.
        let a = compile("a", 0).unwrap();
        let after = a.dfa.next_state(a.start, b'a');
        assert!(
            !a.dfa.is_match_state(after),
            "matches are delayed by one byte"
        );
        assert!(a.dfa.is_match_state(a.dfa.next_state(after, b'x')));
        assert!(a.dfa.is_match_state(a.dfa.next_eoi_state(after)));
        let foo_end = compile("foo$", 0).unwrap();
        let sid = b"foo"
            .iter()
            .fold(foo_end.start, |s, &b| foo_end.dfa.next_state(s, b));
        assert!(foo_end.dfa.is_match_state(foo_end.dfa.next_eoi_state(sid)));
        let anchored = compile("^foo", 0).unwrap();
        assert!(
            anchored
                .dfa
                .is_dead_state(anchored.dfa.next_state(anchored.start, b'b'))
        );
        let any = compile(".*", 0).unwrap();
        assert!(any.dfa.is_match_state(any.dfa.next_state(any.start, b'q')));
    }

    proptest! {
        #[test]
        fn property_delta_equivalence(
            text in text_strategy(),
            cuts in prop::collection::vec(any::<usize>(), 0..12),
            which in 0_usize..3,
        ) {
            let pattern = [r"\bsleep\s*\(", "git commit -m", "TODO"][which];
            let deltas = split(text.as_bytes(), &cuts);
            let expected = oracle(pattern)
                .shortest_match(text.as_bytes())
                .map(|end| delta_containing(&deltas, end - 1));
            prop_assert_eq!(fire_delta(pattern, &deltas), expected);
        }

        #[test]
        fn anchors_at_delta_boundaries(
            text in text_strategy(),
            cuts in prop::collection::vec(any::<usize>(), 0..12),
            which in 0_usize..5,
        ) {
            let pattern = ["^foo", "(?m)^foo$", r"foo\b", "foo$", r"\bs"][which];
            let deltas = split(text.as_bytes(), &cuts);
            let re = oracle(pattern);
            let mut end = 0;
            let expected = deltas.iter().position(|delta| {
                end += delta.len();
                re.is_match(&text.as_bytes()[..end])
            });
            prop_assert_eq!(fire_delta(pattern, &deltas), expected);
        }

        #[test]
        fn excerpt_is_lead_aligned_suffix(
            text in prop::collection::vec(any::<char>(), 0..400).prop_map(String::from_iter),
            cuts in prop::collection::vec(any::<usize>(), 0..12),
        ) {
            let mut state = StreamState::new();
            let mut fires = Vec::new();
            for delta in split(text.as_bytes(), &cuts) {
                state.feed(delta, &mut fires);
            }
            let mut start = text.len().saturating_sub(EXCERPT_BYTES);
            while !text.is_char_boundary(start) {
                start += 1;
            }
            prop_assert_eq!(state.excerpt(), &text[start..]);
        }

        #[test]
        fn node_limit_boundary(n in 1_usize..200, m in 1_usize..200) {
            let result = bounded_nfa(&format!("(a{{{n}}}){{{m}}}"));
            let node = matches!(result, Err(SkipReason::NodeLimit));
            if n * m > NODE_LIMIT {
                prop_assert!(node);
            } else if n * m < NODE_LIMIT - 100 {
                prop_assert!(!node);
            }
        }

        #[test]
        fn too_long_is_first(src in "[a-z(\\[é]{1025,1100}") {
            let skip = compile(&src, 0).unwrap_err();
            prop_assert_eq!(skip.reason, SkipReason::TooLong);
        }
    }
}
