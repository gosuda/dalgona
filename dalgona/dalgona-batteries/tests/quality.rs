// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Behavior tests for the quality battery detectors and codemod offers.

use std::error::Error;
use std::path::Path;

use dal_tools::parse::{codemod_delete_commented_code, codemod_rethrow_empty_catch};
use dalgona_batteries::quality::offers::{Offer, delete_offers, rethrow_offers};
use dalgona_batteries::quality::{
    QualityConfig, QualityConfigError,
    detectors::{
        collapse::{Source as CollapseSource, State as CollapseState},
        control_leak::State as LeakState,
        fabricated_call,
        repetitive_turns::TurnHistory,
    },
};

type TestResult = Result<(), Box<dyn Error>>;
struct RethrowCase {
    path: &'static str,
    source: &'static [u8],
    replacement: &'static str,
    expected: &'static [u8],
    expected_line: u32,
}

#[test]
fn quality_config_rejects_unknown_keys_and_non_tables() -> TestResult {
    assert_eq!(
        QualityConfig::parse_config(None),
        Ok(QualityConfig {
            detectors_enabled: true
        })
    );
    let empty = toml::Value::Table(toml::map::Map::default());
    assert_eq!(
        QualityConfig::parse_config(Some(&empty)),
        Ok(QualityConfig {
            detectors_enabled: true
        })
    );
    let non_table = toml::Value::String("enabled".to_owned());
    assert_eq!(
        QualityConfig::parse_config(Some(&non_table)),
        Err(QualityConfigError::NotATable)
    );
    let unknown: toml::Value = toml::from_str("enabled = true")?;
    assert!(matches!(
        QualityConfig::parse_config(Some(&unknown)),
        Err(QualityConfigError::UnknownKey { key }) if key.as_ref() == "enabled"
    ));
    Ok(())
}

#[test]
fn collapse_run_fires_once_and_latches() {
    let mut state = CollapseState::new(CollapseSource::Prose);
    let stream = "!".repeat(300);
    assert!(state.feed(&stream).is_some());
    assert!(state.latched());
    assert!(state.feed("!!!").is_none());
}

fn collapse_fire(text: &str) -> Option<(&'static str, u64)> {
    CollapseState::new(CollapseSource::Prose)
        .feed(text)
        .map(|fire| (fire.reason, fire.anomaly_start_offset))
}

fn assert_run_threshold(units: &[&str], threshold: usize) {
    for unit in units {
        assert_eq!(
            collapse_fire(&unit.repeat(threshold - 1)),
            None,
            "{unit:?} x {}",
            threshold - 1
        );
        assert_eq!(
            collapse_fire(&unit.repeat(threshold)),
            Some(("dominant_run", 0)),
            "{unit:?} x {threshold}"
        );
        assert_eq!(
            collapse_fire(&unit.repeat(threshold + 1)),
            Some(("dominant_run", 0)),
            "{unit:?} x {}",
            threshold + 1
        );
    }
}

#[test]
fn a_punctuation_run_fires_at_exactly_256_repeats() {
    assert_run_threshold(&["!", "?", "$", "%", "&", "@"], 256);
}

#[test]
fn a_cjk_run_fires_at_exactly_224_repeats() {
    assert_run_threshold(&["\u{4f60}", "\u{ac00}"], 224);
}

#[test]
fn a_dominant_run_resumes_across_gaps_only_while_gap_times_eight_fits_the_count() {
    let with_gap = |count: usize, gap: usize| {
        collapse_fire(&format!(
            "{}{}{}",
            "!".repeat(count),
            " ".repeat(gap),
            "!".repeat(300 - count)
        ))
    };
    for (count, widest_gap) in [(224_usize, 28_usize), (160, 20), (96, 12)] {
        assert_eq!(
            with_gap(count, widest_gap),
            Some(("dominant_run", 0)),
            "{widest_gap} blanks after {count}"
        );
        assert_eq!(
            with_gap(count, widest_gap + 1),
            None,
            "{} blanks after {count} end the run",
            widest_gap + 1
        );
    }
}

const LEAK_TOKEN: &str = "<|endoftext|>";

/// Whether a leak detector that read `stream` and then `trailing` more bytes of
/// prose corroborates the collapse the same `stream` causes.
fn leak_corroborates(stream: &str, trailing: usize) -> Result<bool, Box<dyn Error>> {
    let mut leak = LeakState::new();
    let _ = leak.feed(stream);
    let _ = leak.feed(&"x".repeat(trailing));
    let fire = CollapseState::new(CollapseSource::Prose)
        .feed(stream)
        .ok_or_else(|| std::io::Error::other("the stream should collapse"))?;
    Ok(leak.corroborates(&fire))
}

#[test]
fn a_leak_corroborates_the_collapse_that_starts_at_its_first_payload() -> TestResult {
    let adjacent = format!("{LEAK_TOKEN}{}", "!".repeat(300));
    assert!(
        leak_corroborates(&adjacent, 0)?,
        "the payload follows the token"
    );
    let unrelated = format!("{LEAK_TOKEN}xxxxx{}", "!".repeat(300));
    assert!(
        !leak_corroborates(&unrelated, 0)?,
        "a collapse that starts after other payload is not this token's"
    );
    Ok(())
}

#[test]
fn a_leak_stops_corroborating_after_more_than_32_blanks_before_the_payload() -> TestResult {
    for (blanks, expected) in [(31_usize, true), (32, true), (33, false)] {
        let stream = format!("{LEAK_TOKEN}{}{}", " ".repeat(blanks), "!".repeat(300));
        assert_eq!(leak_corroborates(&stream, 0)?, expected, "{blanks} blanks");
    }
    Ok(())
}

#[test]
fn a_leak_stops_corroborating_one_byte_past_its_2048_byte_window() -> TestResult {
    let adjacent = format!("{LEAK_TOKEN}{}", "!".repeat(300));
    let window_edge = LEAK_TOKEN.len() + 2048 - adjacent.len();
    assert!(
        leak_corroborates(&adjacent, window_edge)?,
        "last offset inside the window"
    );
    assert!(
        !leak_corroborates(&adjacent, window_edge + 1)?,
        "one byte past the window"
    );
    Ok(())
}

#[test]
fn control_token_run_fires_only_outside_a_fence() {
    let mut plain = LeakState::new();
    let mut plain_fired = false;
    for token in ["<|endoftext|>", "<|endoftext|>", "<|endoftext|>"] {
        plain_fired |= plain.feed(token).is_some();
    }
    assert!(plain_fired);

    let mut quoted = LeakState::new();
    let _ = quoted.feed("```\n");
    let mut quoted_fired = false;
    for token in ["<|endoftext|>", "<|endoftext|>", "<|endoftext|>"] {
        quoted_fired |= quoted.feed(token).is_some();
    }
    assert!(!quoted_fired);
}

#[test]
fn collapse_fire_needs_correlated_token_evidence_to_be_reclassified() -> TestResult {
    let mut collapse = CollapseState::new(CollapseSource::Prose);
    let fire = collapse
        .feed(&"!".repeat(300))
        .ok_or_else(|| std::io::Error::other("the dominant run should fire"))?;
    assert!(!LeakState::new().corroborates(&fire));
    Ok(())
}

#[test]
fn similar_turns_fire_then_latch_until_reset() {
    let mut history = TurnHistory::restore(&[]);
    let first = "the quick brown fox jumps over the lazy dog near the river bank today";
    let second = "the quick brown fox jumps over the lazy dog near the river bank today!";
    let third = "the quick brown fox jumps over the lazy dog near the river bank today?";
    assert!(history.commit(first).is_none());
    assert!(history.commit(second).is_none());
    assert!(history.commit(third).is_some());
    assert!(history.commit(third).is_none());
    history.reset();
    assert!(history.commit(first).is_none());
}

#[test]
fn dissimilar_turn_rearms_and_the_next_run_reports_again() {
    let mut history = TurnHistory::restore(&[]);
    let base = "the quick brown fox jumps over the lazy dog near the river bank today";
    assert!(history.commit(base).is_none());
    assert!(history.commit(&format!("{base}!")).is_none());
    assert!(
        history.commit(&format!("{base}?")).is_some(),
        "the third similar turn reports"
    );
    let other = "a completely different reply about database indexes and query plans";
    assert!(
        history.commit(other).is_none(),
        "a dissimilar turn never reports"
    );
    assert!(
        history.commit(&format!("{other}!")).is_none(),
        "the first similar turn after the reset stays quiet"
    );
    assert!(
        history.commit(&format!("{other}?")).is_some(),
        "the new three-turn run reports on its third turn"
    );
}

#[test]
fn degenerate_megabyte_inputs_leave_lanes_bounded_and_report_only() {
    let mebibyte = 1024 * 1024;
    let repeated = "z".repeat(mebibyte);
    let whitespace = " \t\n".repeat(mebibyte / 3);
    let paragraphs = "alpha beta gamma\ndelta epsilon zeta\n\n".repeat(mebibyte / 30);
    for input in [&repeated, &whitespace, &paragraphs] {
        let mut collapse = CollapseState::new(CollapseSource::Prose);
        assert!(feed_all(&mut collapse, input) <= 1, "collapse {input:?}");
        let mut tool_collapse = CollapseState::new(CollapseSource::Tool);
        assert!(feed_all(&mut tool_collapse, input) <= 1, "tool collapse");
    }
    let tokens = "<|endoftext|> ".repeat(mebibyte / 14);
    let mut leak = LeakState::new();
    assert!(feed_all(&mut leak, &tokens) <= 1, "control-token leak");
    let mut history = TurnHistory::restore(&[]);
    // Distinct non-hex words: one repeated word has no trigrams and never
    // fires by design, and hex-like runs normalize to a single token.
    let alphabet: Vec<char> = "ghijklmnopqrstuvwxyz".chars().collect();
    let cycle: String = (0..8000).fold(String::new(), |mut acc, n| {
        let (a, b, c) = (n / 400, n / 20 % 20, n % 20);
        acc.push(alphabet[a]);
        acc.push(alphabet[b]);
        acc.push(alphabet[c]);
        acc.push(' ');
        acc
    });
    let turn = cycle.repeat(mebibyte / cycle.len() + 1);
    assert!(turn.len() >= mebibyte);
    let second = format!("{turn}!");
    let third = format!("{turn}?");
    assert!(history.commit(&turn).is_none());
    assert!(history.commit(&second).is_none());
    assert!(history.commit(&third).is_some());
    assert!(
        history.commit(&third).is_none(),
        "the lane latches after one report per run"
    );
}

/// Feeds `input` in bounded deltas and counts the fires.
fn feed_all(state: &mut impl Feed, input: &str) -> usize {
    let delta_bytes = 8 * 1024;
    let mut fires = 0;
    let mut rest = input;
    while !rest.is_empty() {
        let end = (rest)
            .char_indices()
            .nth(delta_bytes)
            .map_or(rest.len(), |(index, _)| index);
        let (delta, tail) = rest.split_at(end);
        fires += usize::from(state.feed(delta).is_some());
        rest = tail;
    }
    fires
}

trait Feed {
    fn feed(&mut self, delta: &str) -> Option<()>;
}

impl Feed for CollapseState {
    fn feed(&mut self, delta: &str) -> Option<()> {
        CollapseState::feed(self, delta).map(|_| ())
    }
}

impl Feed for LeakState {
    fn feed(&mut self, delta: &str) -> Option<()> {
        LeakState::feed(self, delta).map(|_| ())
    }
}

#[test]
fn unavailable_tool_patterns_match_the_supported_notices() -> TestResult {
    let rule = fabricated_call::rule()?;
    let mut patterns = Vec::new();
    for pattern in &rule.patterns {
        patterns.push(regex_automata::meta::Regex::new(pattern.as_ref())?);
    }
    assert!(
        patterns
            .iter()
            .any(|pattern| pattern.is_match("<unavailable-tool-call>"))
    );
    assert!(patterns.iter().any(|pattern| {
        pattern.is_match("[called tool 'patch' (no longer available in this session)]")
    }));
    assert!(
        !patterns
            .iter()
            .any(|pattern| pattern.is_match("ordinary prose"))
    );
    Ok(())
}

fn comments_offered(path: &str, source: &[u8]) -> Result<Vec<Offer>, Box<dyn Error>> {
    let matches = codemod_delete_commented_code(Path::new(path), source)?;
    Ok(delete_offers("1", path, source, &matches, 0))
}

#[test]
fn commented_code_offers_cover_complete_comment_lines_in_supported_grammars() -> TestResult {
    for (path, source) in [
        ("src/a.ml", b"(* let f x = x + 1 in f 2 *)\n".as_slice()),
        ("src/a.py", b"# def unused(): pass\n".as_slice()),
        ("src/a.cpp", b"// int old() { return 1; }\n".as_slice()),
    ] {
        let offers = comments_offered(path, source)?;
        assert_eq!(offers.len(), 1, "{path}");
        assert_eq!(offers[0].before.as_bytes(), source, "{path}");
        assert!(offers[0].after.is_empty(), "{path}");
    }
    Ok(())
}

#[test]
fn shared_code_and_prose_comments_do_not_create_delete_offers() -> TestResult {
    let shared = b"let g () = 1  (* let h = 2 *)\n";
    assert_eq!(comments_offered("src/a.ml", shared)?.len(), 0);
    let prose = b"(* this helper normalizes the input *)\n";
    assert_eq!(comments_offered("src/a.ml", prose)?.len(), 0);
    Ok(())
}

fn empty_catch_offers(path: &str, source: &[u8]) -> Result<Vec<Offer>, Box<dyn Error>> {
    let matches = codemod_rethrow_empty_catch(Path::new(path), source)?;
    Ok(rethrow_offers("1", path, source, &matches, 0))
}

#[test]
fn empty_catch_offers_produce_language_specific_rethrow_edits() -> TestResult {
    let cases = [
        RethrowCase {
            path: "src/a.py",
            source: b"try:\n    run()\nexcept Exception:\n    pass\n",
            replacement: "except Exception: raise\n",
            expected: b"try:\n    run()\nexcept Exception: raise\n",
            expected_line: 3,
        },
        RethrowCase {
            path: "src/a.cpp",
            source: b"void example() {\n  try {\n    run();\n  }\n  catch (...) {}\n}\n",
            replacement: "catch (...) { throw; }\n",
            expected: b"void example() {\n  try {\n    run();\n  }\n  catch (...) { throw; }\n}\n",
            expected_line: 5,
        },
        RethrowCase {
            path: "src/a.ml",
            source: b"let r =\n  try f () with\n  | E -> ()\n  | _ -> ()\n",
            replacement: "| exn -> raise exn\n",
            expected: b"let r =\n  try f () with\n  | E -> ()\n  | exn -> raise exn\n",
            expected_line: 4,
        },
    ];
    for case in cases {
        let offers = empty_catch_offers(case.path, case.source)?;
        assert_eq!(offers.len(), 1, "{}", case.path);
        assert_eq!(offers[0].after, case.replacement, "{}", case.path);
        let start = usize::try_from(offers[0].byte_start)?;
        let end = usize::try_from(offers[0].byte_end)?;
        assert_eq!(
            &case.source[start..end],
            offers[0].before.as_bytes(),
            "{}",
            case.path
        );
        assert_eq!(offers[0].line_start, case.expected_line, "{}", case.path);
        let mut applied =
            Vec::with_capacity(case.source.len() - (end - start) + offers[0].after.len());
        applied.extend_from_slice(&case.source[..start]);
        applied.extend_from_slice(offers[0].after.as_bytes());
        applied.extend_from_slice(&case.source[end..]);
        assert_eq!(applied, case.expected, "{}", case.path);
    }
    let shared_line = b"void example() { try { run(); } catch (...) {} }\n";
    assert_eq!(empty_catch_offers("src/a.cpp", shared_line)?.len(), 0);
    Ok(())
}
