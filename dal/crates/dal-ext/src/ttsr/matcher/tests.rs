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
        compile(&"a".repeat(1024), 0).map_or_else(|s| s.reason != SkipReason::TooLong, |_| true)
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
    assert!(begun.elapsed() < Duration::from_secs(30));
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
        fn too_long_is_first(src in "[a-z(\\[é]{1025,1100}") {
            let skip = compile(&src, 0).unwrap_err();
            prop_assert_eq!(skip.reason, SkipReason::TooLong);
        }
}

// Sealed case budget: one boundary compile costs ~0.5 s, so the default 256
// cases would eat the suite. The ranges stay full; fewer samples still pin
// both sides of the node-limit band.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn node_limit_boundary(n in 1_usize..200, m in 1_usize..200) {
        let result = compile(&format!("(a{{{n}}}){{{m}}}"), 0);
        let node = matches!(&result, Err(s) if s.reason == SkipReason::NodeLimit);
        if n * m > NODE_LIMIT {
            prop_assert!(node);
        } else if n * m < NODE_LIMIT - 100 {
            prop_assert!(!node);
        }
    }
}
