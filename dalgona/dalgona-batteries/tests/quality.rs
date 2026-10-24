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

/// Guard offers reach the model, through the real host.
///
/// The real guard observes a real `patch` that adds a commented-out line;
/// the real quality battery settles the turn into a `quality_offers`
/// record. A probe model at the provider boundary records every request's
/// model-visible text, then forwards it to the scripted provider. The
/// offer line must reach the next request exactly once, and once applied it
/// must not come back as a fresh notice.
mod host_offers {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::fmt::Write as _;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use dal_agent::ext::grants::{GrantKey, GrantStore};
    use dal_agent::ext::{
        BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
    };
    use dal_agent::{Delivery, Env, Host, Product, SessionRef};
    use dal_core::{
        Caps, ClientId, Command, Config, ConfigProduct, ContextItem, Expect, Family, GuardSection,
        ModelId, ModelRequest, ModelRoute, PageReq, Part, ServiceSet, SessionId, ThinkingLevel,
        Timestamp, UpdateKind, Workspace,
    };
    use dal_tools::guard::findings_json;
    use dal_tools::{Calibration, FindingsHandle, GuardConfig, ToolsConfig, guard_extension};
    use dalgona_batteries::quality::{QualityConfig, quality};

    use super::TestResult;

    const WAIT: Duration = Duration::from_secs(60);
    const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;
    /// The seeded file. The guard's commented-out check reads added lines
    /// from the staged diff hunks, and a `create` stages no hunks
    /// (`patch/write/stage.rs` `Edit::Create`), so the scripted patch must
    /// update an existing file to produce a real finding.
    const SEEDED: &str = "keep = 1\n";
    /// The file after the scripted patch: one added commented-out code line.
    const EDITED: &str = "# def unused(): pass\nkeep = 1\n";

    struct TestRoot(PathBuf);

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Model-visible text of one provider request.
    #[derive(Clone, Debug)]
    struct Seen {
        /// The system prompt, which prompt sections render into.
        system: String,
        /// User text after the last assistant or tool-result item: the
        /// current turn's input, including any `before_turn` reminder.
        current: String,
    }

    /// Every provider request, in request order.
    #[derive(Default)]
    struct Probe {
        seen: Mutex<Vec<Seen>>,
    }

    impl Probe {
        fn seen(&self) -> Vec<Seen> {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    fn seen_of(request: &ModelRequest) -> Seen {
        let start = request
            .context
            .iter()
            .rposition(|item| !matches!(item, ContextItem::User { .. }))
            .map_or(0, |index| index + 1);
        let mut current = String::new();
        for item in &request.context[start..] {
            if let ContextItem::User { parts } = item {
                for part in parts {
                    if let Part::Text { text } = part {
                        current.push_str(text);
                        current.push('\n');
                    }
                }
            }
        }
        Seen {
            system: request.system.to_string(),
            current,
        }
    }

    impl ModelHandler for Probe {
        fn run<'a>(
            &'a self,
            request: ModelRequest,
            cx: ModelCx<'a>,
        ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
            Box::pin(async move {
                self.seen
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(seen_of(&request));
                let mut forwarded = request;
                forwarded.model = ModelRoute::Api {
                    provider: None,
                    family: Family::Chat,
                    model: "gpt-6-luna".into(),
                };
                cx.forward(forwarded, &[]).await
            })
        }
    }

    fn caps() -> Caps {
        Caps {
            context_window: Some(100_000),
            thinking: Box::new([ThinkingLevel::Off]),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        }
    }

    fn calls_step(calls: &[(&str, &str, String)]) -> String {
        let started = calls
            .iter()
            .map(|(id, name, _)| {
                format!(r#"{{"type":"tool_call_started","id":"{id}","name":"{name}"}}"#)
            })
            .collect::<Vec<_>>()
            .join(",");
        let done = calls
            .iter()
            .map(|(id, name, args)| {
                format!(
                    r#"{{"id":"{id}","name":"{name}","args":{{"kind":"parsed","value":{args}}}}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"kind":"events","events":[{started},{{"type":"tool_calls_done","calls":[{done}]}},{USAGE},{{"type":"stop","reason":"tool_use"}}]}}"#
        )
    }

    fn text_step() -> String {
        format!(
            r#"{{"kind":"events","events":[{{"type":"text_delta","text":"done"}},{{"type":"tool_calls_done","calls":[]}},{USAGE},{{"type":"stop","reason":"end_turn"}}]}}"#
        )
    }

    fn patch_args(edited: &str) -> Result<String, Box<dyn std::error::Error>> {
        let value = sonic_rs::json!({
            "changes": [{"path": "src/a.py", "old": SEEDED, "new": edited}]
        });
        Ok(sonic_rs::to_string(&value)?)
    }

    /// What one prompt published, up to and including its `TurnEnded`.
    #[derive(Debug, Default)]
    struct Turn {
        id: Option<dal_core::TurnId>,
        stop: Option<dal_core::Stop>,
        /// `(is_error, text)` of every settled tool call.
        settled: Vec<(bool, String)>,
        /// `(kind, text)` of every notice.
        notices: Vec<(String, String)>,
    }

    impl Turn {
        fn quality_panel(&self) -> String {
            self.notices
                .iter()
                .filter(|(kind, _)| kind == "quality")
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    /// Prompts once and collects the turn's updates through `TurnEnded`.
    /// The actor publishes `TurnEnded` before the driver runs the turn-end
    /// hooks, and the next opening waits for those hooks, so a settle
    /// notice of one turn is collected by the next prompt.
    async fn prompt(
        stage: &str,
        agent: &dal_agent::Agent,
        subscription: &mut dal_agent::Subscription,
        text: &str,
    ) -> Result<Turn, Box<dyn std::error::Error>> {
        agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text { text: text.into() }],
            })
            .await
            .map_err(|error| format!("{stage}: prompt refused: {error}"))?;
        let mut turn = Turn::default();
        loop {
            let Ok(delivery) = tokio::time::timeout(WAIT, subscription.next()).await else {
                return Err(
                    format!("{stage}: no TurnEnded within {WAIT:?}; so far {turn:?}").into(),
                );
            };
            let Some(delivery) = delivery else {
                return Err(format!("{stage}: subscription closed; so far {turn:?}").into());
            };
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::ToolSettled { outcome, .. } => turn
                    .settled
                    .push((outcome.is_error, outcome.text.to_string())),
                UpdateKind::Notice(notice) => turn
                    .notices
                    .push((notice.kind.to_string(), notice.text.to_string())),
                UpdateKind::TurnEnded {
                    turn: turn_id,
                    stop,
                } => {
                    turn.id = Some(*turn_id);
                    turn.stop = Some(*stop);
                    return Ok(turn);
                }
                _ => {}
            }
        }
    }

    /// Waits for the guard's own turn-end hook to publish `turn`'s findings
    /// through the public handle quality reads, then returns them as the
    /// `guard://findings` document. The hook runs after `TurnEnded`.
    async fn guard_findings(
        findings: &FindingsHandle,
        session: &SessionId,
        turn: dal_core::TurnId,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let published = tokio::time::timeout(WAIT, async {
            loop {
                if let Some(last) = findings.last(session).filter(|last| last.turn == turn) {
                    return last;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| format!("turn 1 (guard): no findings for {turn:?} within {WAIT:?}"))?;
        Ok(findings_json(&published))
    }

    /// Asserts the turn ended normally with every tool call succeeding.
    fn assert_clean(stage: &str, turn: &Turn, calls: usize) {
        assert_eq!(
            turn.stop,
            Some(dal_core::Stop::EndTurn),
            "{stage}: the turn must end normally: {turn:?}"
        );
        assert_eq!(
            turn.settled.len(),
            calls,
            "{stage}: settled calls: {turn:?}"
        );
        assert!(
            turn.settled.iter().all(|(is_error, _)| !is_error),
            "{stage}: a tool call failed: {turn:?}"
        );
    }

    /// Runs one turn to its end and returns it with its first provider request.
    async fn first_request_of(
        stage: &str,
        probe: &Probe,
        agent: &dal_agent::Agent,
        subscription: &mut dal_agent::Subscription,
        text: &str,
    ) -> Result<(Turn, Seen), Box<dyn std::error::Error>> {
        let before = probe.seen().len();
        let turn = prompt(stage, agent, subscription, text).await?;
        let seen = probe
            .seen()
            .get(before)
            .cloned()
            .ok_or_else(|| format!("{stage}: no provider request; {turn:?}"))?;
        Ok((turn, seen))
    }

    /// A real host with the enabled guard, the real tools wired to it, the
    /// real quality battery granted `fs.read`, and the probe model.
    struct Rig {
        _root: TestRoot,
        host: Host,
        agent: dal_agent::Agent,
        subscription: dal_agent::Subscription,
        session: SessionId,
        probe: Arc<Probe>,
        findings: FindingsHandle,
        workspace: PathBuf,
    }

    /// Seeds `src/a.py` with [`SEEDED`] and opens a session whose scripted
    /// model answers `steps`, one per provider request.
    async fn rig(steps: &[String]) -> Result<Rig, Box<dyn std::error::Error>> {
        rig_seeded(&["src/a.py"], steps).await
    }

    /// Seeds every workspace-relative path in `seeds` with [`SEEDED`] and
    /// opens a session whose scripted model answers `steps`.
    async fn rig_seeded(
        seeds: &[&str],
        steps: &[String],
    ) -> Result<Rig, Box<dyn std::error::Error>> {
        let root = TestRoot(
            std::env::temp_dir().join(format!("dalgona-quality-offers-{}", SessionId::new_v7())),
        );
        let data = root.0.join("data");
        let workspace = root.0.join("workspace");
        fs::create_dir_all(&data)?;
        for seed in seeds {
            let path = workspace.join(seed);
            fs::create_dir_all(path.parent().ok_or("seed path has no parent")?)?;
            fs::write(path, SEEDED)?;
        }
        let fixture = data.join("script.jsonl");
        fs::write(&fixture, steps.join("\n") + "\n")?;
        let user = format!(
            "model = \"kit/probe\"\napproval = \"all\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
            fixture.display()
        );
        let config = Config::load(ConfigProduct::Dalgona, &data, "", Some(&user))?;

        let section = GuardSection {
            enabled: true,
            ..GuardSection::default()
        };
        let guard = guard_extension(GuardConfig::from_section(&section, &Calibration::none())?)?;
        let tools = dal_tools::extension(ToolsConfig {
            observer: Some(Arc::clone(&guard.observer)),
            edit_style: dal_core::EditStyleInput::Scalar("replace".into()),
            ..ToolsConfig::default()
        })?;
        let findings = guard.findings;
        let quality = quality(
            QualityConfig {
                detectors_enabled: false,
            },
            findings.clone(),
            guard.observer,
        )?;
        // The bundled quality battery reads offered files through the gated
        // `fs.read` service; grant its real code identity at this workspace,
        // as a user would, so offers are built through the real service.
        let grants = GrantStore::new(data.clone());
        let quality_key = GrantKey::for_extension(&quality, &workspace)
            .ok_or("the quality extension has no code identity")?;
        let granted = grants
            .grant(
                quality_key,
                ClientId::new("quality-offers"),
                Timestamp::now(),
            )
            .await?;
        assert!(granted, "the quality fs.read grant must be stored");
        // The guard's own hooks read through `fs.read` too; grant it when its
        // code identity is grantable.
        if let Some(guard_key) = GrantKey::for_extension(&guard.extension, &workspace) {
            grants
                .grant(guard_key, ClientId::new("quality-offers"), Timestamp::now())
                .await?;
        }
        let probe = Arc::new(Probe::default());
        let kit = ExtensionBuilder::new("kit", "0.1.0", ServiceSet::default())?
            .model(ModelRecord {
                id: ModelId::parse("kit/probe")?,
                caps: caps(),
                handler: Arc::clone(&probe) as Arc<dyn ModelHandler>,
                export: None,
            })
            .build()?;
        let product = Product {
            name: "dalgona",
            data_root: data,
            defaults: "",
            extensions: vec![guard.extension, tools, quality, kit],
            bundled: Vec::new(),
        };
        let env = Env {
            vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
            cwd: workspace.clone(),
            sandbox_helper: None,
        };
        let host = Host::start(product, config, env).await?;
        let agent = host
            .open(
                SessionRef::New {
                    workspace: Workspace::new(workspace.clone())?,
                    name: None,
                },
                ClientId::new("quality-offers"),
            )
            .await?;
        let subscription = agent.subscribe(None)?;
        let session = agent.view(PageReq::default())?.session.id;
        Ok(Rig {
            _root: root,
            host,
            agent,
            subscription,
            session,
            probe,
            findings,
            workspace,
        })
    }

    /// Commented-out definitions added by one patch, separated by live code
    /// so each is its own whole-line offer: `q-1-<n>` on line `2n - 1`.
    const MANY_OFFERS: usize = 12;

    fn many_commented_lines() -> String {
        let mut text = String::new();
        for index in 0..MANY_OFFERS {
            let _ = write!(
                text,
                "# def unused_{index}(): pass\nlive_{index} = {index}\n"
            );
        }
        text.push_str(SEEDED);
        text
    }

    fn many_offer_line(number: usize) -> String {
        let line = 2 * number - 1;
        format!("q-1-{number} delete-commented-code src/a.py:{line}-{line}")
    }

    /// One turn's model notice is bounded: a patch that yields more offers
    /// than one notice lists shows the first ones, keeps the rest pending
    /// without marking them shown, and lists them in the next turn.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_large_offer_backlog_is_listed_in_bounded_turns() -> TestResult {
        let edited = many_commented_lines();
        let steps = [
            calls_step(&[("c-patch", "patch", patch_args(&edited)?)]),
            text_step(),
            text_step(),
            text_step(),
            text_step(),
        ];
        let Rig {
            _root,
            host,
            agent,
            mut subscription,
            session: _,
            probe,
            findings: _,
            workspace,
        } = rig(&steps).await?;

        let turn_one = prompt("turn 1 (patch)", &agent, &mut subscription, "edit the file").await?;
        assert_clean("turn 1 (patch)", &turn_one, 1);
        assert_eq!(fs::read_to_string(workspace.join("src/a.py"))?, edited);

        // Turn 2: the human settle notice lists every offer the record holds.
        let (turn, first) =
            first_request_of("turn 2", &probe, &agent, &mut subscription, "any offers?").await?;
        assert_clean("turn 2", &turn, 0);
        let panel = turn.quality_panel();
        for number in 1..=MANY_OFFERS {
            assert!(
                panel.contains(&many_offer_line(number)),
                "turn 1 (settle): the record must hold {}: {turn:?}",
                many_offer_line(number)
            );
        }
        // The model notice lists exactly the first ten, once each, names
        // the two left pending, and copies no offer payload.
        let listed: Vec<usize> = (1..=MANY_OFFERS)
            .filter(|number| first.current.contains(&many_offer_line(*number)))
            .collect();
        assert_eq!(listed, (1..=10).collect::<Vec<_>>(), "{first:?}");
        assert_eq!(
            first.current.matches("codemod offer(s)").count(),
            1,
            "{first:?}"
        );
        let notice = quality_notice(&first);
        assert!(
            notice.contains("2 more"),
            "pending count missing: {first:?}"
        );
        assert!(
            !notice.contains("def unused_"),
            "the notice must not copy the offered bytes: {first:?}"
        );
        assert!(!first.system.contains("q-1-"), "{first:?}");
        assert_notice_bounded("turn 2", &first);

        // Turn 3: the two offers no notice listed were not marked shown, so
        // they are listed now, and the ten already shown are not.
        let (turn, second) =
            first_request_of("turn 3", &probe, &agent, &mut subscription, "and now?").await?;
        assert_clean("turn 3", &turn, 0);
        let listed: Vec<usize> = (1..=MANY_OFFERS)
            .filter(|number| second.current.contains(&many_offer_line(*number)))
            .collect();
        assert_eq!(listed, vec![11, 12], "{second:?}");
        assert!(
            quality_notice(&second).ends_with("{\"id\": \"<id>\"}.\n"),
            "no pending line may follow the apply line: {second:?}"
        );

        // Turn 4: every offer has been listed once; no notice remains.
        let (turn, third) = first_request_of(
            "turn 4",
            &probe,
            &agent,
            &mut subscription,
            "anything else?",
        )
        .await?;
        assert_clean("turn 4", &turn, 0);
        assert!(!third.current.contains("codemod offer(s)"), "{third:?}");

        drop(subscription);
        drop(agent);
        drop(host);
        Ok(())
    }

    /// The explicit byte bound of one turn's model notice: the quality
    /// battery's `NOTICE_BYTES`.
    const NOTICE_BYTES: usize = 4096;

    /// Quality's own notice inside the turn's reminder: from its
    /// `quality: ` header through its apply line and an optional pending
    /// line, the next line when it starts with its count. The guard's
    /// before-turn report joins the same reminder, so byte, path, and
    /// payload assertions read this slice only.
    fn quality_notice(seen: &Seen) -> &str {
        const APPLY: &str = "Apply one with quality_apply {\"id\": \"<id>\"}.\n";
        let text = seen.current.as_str();
        let Some(start) = text.find("quality: ") else {
            return "";
        };
        let Some(apply) = text[start..].find(APPLY) else {
            return &text[start..];
        };
        let mut end = start + apply + APPLY.len();
        if let Some(line) = text[end..].lines().next()
            && line.starts_with(|first: char| first.is_ascii_digit())
        {
            end = (end + line.len() + 1).min(text.len());
        }
        &text[start..end]
    }

    /// Asserts quality's notice stays within the notice byte bound.
    fn assert_notice_bounded(stage: &str, seen: &Seen) {
        let notice = quality_notice(seen).len();
        assert!(
            notice <= NOTICE_BYTES,
            "{stage}: the quality notice holds {notice} bytes, over {NOTICE_BYTES}: {seen:?}"
        );
    }

    /// One `old`/`new` change per path, each adding the one commented line.
    fn patch_many(paths: &[String]) -> Result<String, Box<dyn std::error::Error>> {
        let changes: Vec<sonic_rs::Value> = paths
            .iter()
            .map(|path| sonic_rs::json!({"path": path, "old": SEEDED, "new": EDITED}))
            .collect();
        Ok(sonic_rs::to_string(
            &sonic_rs::json!({ "changes": changes }),
        )?)
    }

    /// Reads updates after `TurnEnded` until quality's turn-end hook
    /// publishes its settle notice, which follows that hook's file reads
    /// and record append. Fails with the stage and everything seen.
    async fn settled_panel(
        stage: &str,
        subscription: &mut dal_agent::Subscription,
        turn: &mut Turn,
    ) -> TestResult {
        while turn.quality_panel().is_empty() {
            let Ok(delivery) = tokio::time::timeout(WAIT, subscription.next()).await else {
                return Err(format!("{stage}: no quality notice within {WAIT:?}; {turn:?}").into());
            };
            let Some(delivery) = delivery else {
                return Err(format!("{stage}: subscription closed; {turn:?}").into());
            };
            if let Delivery::Update(update) = delivery
                && let UpdateKind::Notice(notice) = &update.kind
            {
                turn.notices
                    .push((notice.kind.to_string(), notice.text.to_string()));
            }
        }
        Ok(())
    }

    /// Every `q-1-<n>` row the turn's input lists, as offer numbers.
    fn listed_numbers(seen: &Seen, rows: &[String]) -> Vec<usize> {
        rows.iter()
            .enumerate()
            .filter(|(_, row)| seen.current.contains(row.as_str()))
            .map(|(index, _)| index + 1)
            .collect()
    }

    /// Offers whose files cannot be read stay pending and are never marked
    /// shown. The bounded scan moves past them, so an offer behind them
    /// still reaches the model, and a failed offer is retried until its
    /// file reads again.
    #[tokio::test(flavor = "multi_thread")]
    async fn unreadable_offers_stay_pending_while_the_scan_reaches_later_offers() -> TestResult {
        // Guard findings, and so offers, follow path order: the eleven
        // `a*` files make q-1-1..q-1-11 and `z.py` makes q-1-12.
        let mut paths: Vec<String> = (0..11).map(|index| format!("src/a{index:02}.py")).collect();
        paths.push("src/z.py".to_owned());
        let rows: Vec<String> = paths
            .iter()
            .enumerate()
            .map(|(index, path)| format!("q-1-{} delete-commented-code {path}:1-1", index + 1))
            .collect();
        let seeds: Vec<&str> = paths.iter().map(String::as_str).collect();
        let mut steps = vec![calls_step(&[("c-patch", "patch", patch_many(&paths)?)])];
        steps.extend((0..5).map(|_| text_step()));
        let Rig {
            _root,
            host,
            agent,
            mut subscription,
            session: _,
            probe,
            findings: _,
            workspace,
        } = rig_seeded(&seeds, &steps).await?;

        let mut turn_one = prompt("turn 1 (patch)", &agent, &mut subscription, "edit them").await?;
        assert_clean("turn 1 (patch)", &turn_one, 1);
        // The files may change only after quality's turn-end hook has read
        // them: its settle notice is the last thing that hook publishes.
        settled_panel("turn 1 (settle)", &mut subscription, &mut turn_one).await?;
        let panel = turn_one.quality_panel();
        assert!(
            rows.iter().all(|row| panel.contains(row.as_str())),
            "turn 1 (settle): the record must hold all twelve offers: {turn_one:?}"
        );
        // Every `a*` file becomes a directory: reading it fails, and it is
        // neither missing nor changed, so its offer is neither fresh nor
        // stale.
        for path in &paths[..11] {
            let path = workspace.join(path);
            fs::remove_file(&path)?;
            fs::create_dir(&path)?;
        }

        // Turn 2 collects turn 1's human notice; its model notice scans the
        // first ten candidates, all unreadable, and lists none.
        let (turn, second) =
            first_request_of("turn 2", &probe, &agent, &mut subscription, "offers?").await?;
        assert_clean("turn 2", &turn, 0);
        assert_eq!(
            listed_numbers(&second, &rows),
            Vec::<usize>::new(),
            "{second:?}"
        );
        assert!(!second.current.contains("codemod offer(s)"), "{second:?}");

        // Turn 3: the scan continues past the failures and lists z.py.
        let (turn, third) =
            first_request_of("turn 3", &probe, &agent, &mut subscription, "offers?").await?;
        assert_clean("turn 3", &turn, 0);
        assert_eq!(listed_numbers(&third, &rows), vec![12], "{third:?}");
        assert_notice_bounded("turn 3", &third);

        // Turn 4: the scan wraps to the unreadable offers; still none listed.
        let (turn, fourth) =
            first_request_of("turn 4", &probe, &agent, &mut subscription, "offers?").await?;
        assert_clean("turn 4", &turn, 0);
        assert_eq!(
            listed_numbers(&fourth, &rows),
            Vec::<usize>::new(),
            "{fourth:?}"
        );

        // Turn 5: a00.py and a10.py read again; neither offer was marked
        // shown. Turn 4's scan stopped at q-1-7, so this scan reads q-1-8
        // to q-1-11, wraps past the shown q-1-12, and reads q-1-1 to
        // q-1-6: both offers are listed, q-1-11 before q-1-1, and the other
        // nine unreadable offers stay pending.
        for restored in [&paths[0], &paths[10]] {
            let path = workspace.join(restored);
            fs::remove_dir(&path)?;
            fs::write(&path, EDITED)?;
        }
        let (turn, fifth) =
            first_request_of("turn 5", &probe, &agent, &mut subscription, "offers?").await?;
        assert_clean("turn 5", &turn, 0);
        assert_eq!(listed_numbers(&fifth, &rows), vec![1, 11], "{fifth:?}");
        let notice = quality_notice(&fifth);
        let (Some(before_wrap), Some(after_wrap)) = (
            notice.find(rows[10].as_str()),
            notice.find(rows[0].as_str()),
        ) else {
            return Err(format!("turn 5: both rows must be listed: {fifth:?}").into());
        };
        assert!(
            before_wrap < after_wrap,
            "turn 5: the notice lists in scan order across the wrap: {fifth:?}"
        );
        assert!(
            notice.contains("\n9 more"),
            "turn 5: nine stay pending: {fifth:?}"
        );
        assert_notice_bounded("turn 5", &fifth);

        drop(subscription);
        drop(agent);
        drop(host);
        Ok(())
    }

    /// A long workspace path is shortened only in the notice's path text:
    /// the row keeps its full id, the notice stays inside its byte bound,
    /// and `quality_apply` still edits the stored full path.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_long_offer_path_is_shortened_only_in_the_notice() -> TestResult {
        let segment = "d".repeat(200);
        let path = format!("src/{segment}/{segment}/{segment}/{segment}/long.py");
        let steps = [
            calls_step(&[("c-patch", "patch", patch_many(std::slice::from_ref(&path))?)]),
            text_step(),
            text_step(),
            calls_step(&[("c-apply", "quality_apply", r#"{"id":"q-1-1"}"#.to_owned())]),
            text_step(),
        ];
        let Rig {
            _root,
            host,
            agent,
            mut subscription,
            session: _,
            probe,
            findings: _,
            workspace,
        } = rig_seeded(&[path.as_str()], &steps).await?;

        let turn_one = prompt("turn 1 (patch)", &agent, &mut subscription, "edit it").await?;
        assert_clean("turn 1 (patch)", &turn_one, 1);

        let (turn, second) =
            first_request_of("turn 2", &probe, &agent, &mut subscription, "offers?").await?;
        assert_clean("turn 2", &turn, 0);
        let notice = quality_notice(&second);
        assert!(
            notice.contains("q-1-1 delete-commented-code src/"),
            "the row keeps its full id and codemod: {second:?}"
        );
        assert!(
            notice.contains(&path[..200]),
            "the row keeps the path's leading text: {second:?}"
        );
        assert!(
            !notice.contains(path.as_str()),
            "the {}-byte path is shortened in the notice: {second:?}",
            path.len()
        );
        assert_notice_bounded("turn 2", &second);

        let (turn, _) = first_request_of(
            "turn 3 (apply)",
            &probe,
            &agent,
            &mut subscription,
            "apply it",
        )
        .await?;
        assert_clean("turn 3 (apply)", &turn, 1);
        assert_eq!(
            fs::read_to_string(workspace.join(&path))?,
            SEEDED,
            "quality_apply must edit the stored full path: {turn:?}"
        );

        drop(subscription);
        drop(agent);
        drop(host);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_offer_reaches_the_next_model_request_once_and_not_after_apply() -> TestResult {
        // One scripted step per provider request, in order: turn 1 edits
        // the file, turn 2 only answers, turn 3 applies, turn 4 answers.
        let steps = [
            calls_step(&[("c-patch", "patch", patch_args(EDITED)?)]),
            text_step(),
            text_step(),
            calls_step(&[("c-apply", "quality_apply", r#"{"id":"q-1-1"}"#.to_owned())]),
            text_step(),
            text_step(),
            text_step(),
        ];
        let Rig {
            _root,
            host,
            agent,
            mut subscription,
            session,
            probe,
            findings,
            workspace,
        } = rig(&steps).await?;

        // The one offer the guard finding must produce, as `offers::panel`
        // lists it.
        let offer_line = "q-1-1 delete-commented-code src/a.py:1-1";

        // Turn 1: the patch adds a commented-out line to the seeded file.
        let turn_one = prompt("turn 1 (patch)", &agent, &mut subscription, "edit the file").await?;
        assert_clean("turn 1 (patch)", &turn_one, 1);
        assert_eq!(
            fs::read_to_string(workspace.join("src/a.py"))?,
            EDITED,
            "turn 1: the patch must apply: {turn_one:?}"
        );
        // Prerequisite: the real guard recorded the commented-out finding
        // quality builds offers from.
        let turn_one_id = turn_one.id.ok_or("turn 1 has no id")?;
        let document = guard_findings(&findings, &session, turn_one_id).await?;
        assert!(
            document.contains(r#""rule":"commented_out_code","path":"src/a.py","line_start":1"#),
            "turn 1 (guard): no commented-out finding on src/a.py:1: {document}"
        );

        // Turn 2: the next opening waits for turn 1's hooks, so this prompt
        // also collects turn 1's human settle notice. The first request
        // carries the offer once in the turn's own input, not in the
        // per-request system prompt.
        let (turn, turn_two) =
            first_request_of("turn 2", &probe, &agent, &mut subscription, "any offers?").await?;
        assert_clean("turn 2", &turn, 0);
        assert!(
            turn.quality_panel().contains(offer_line),
            "turn 1 (settle): the quality record notice must list {offer_line}: {turn:?}"
        );
        assert_eq!(
            turn_two.current.matches(offer_line).count(),
            1,
            "the offer must reach the model once: {turn_two:?}"
        );
        assert!(!turn_two.system.contains(offer_line), "{turn_two:?}");

        // Turn 3: the offer is still fresh, but it was already shown, so the
        // turn's input carries no second notice. The model applies it.
        let (turn, turn_three) = first_request_of(
            "turn 3 (apply)",
            &probe,
            &agent,
            &mut subscription,
            "apply it",
        )
        .await?;
        assert!(
            !turn_three.current.contains(offer_line),
            "a shown offer must not be presented again: {turn_three:?}"
        );
        assert!(!turn_three.system.contains(offer_line), "{turn_three:?}");
        assert!(
            !turn_three.current.contains("codemod offer(s)"),
            "no offer notice may reach turn 3, under any id: {turn_three:?}"
        );
        assert_clean("turn 3 (apply)", &turn, 1);
        assert_eq!(
            fs::read_to_string(workspace.join("src/a.py"))?,
            SEEDED,
            "turn 3: quality_apply must delete the offered line: {turn:?}"
        );

        // Turn 4: the applied offer is not presented as fresh.
        let (turn, turn_four) = first_request_of(
            "turn 4",
            &probe,
            &agent,
            &mut subscription,
            "anything else?",
        )
        .await?;
        assert_clean("turn 4", &turn, 0);
        assert!(
            !turn_four.current.contains(offer_line),
            "an applied offer must not be presented again: {turn_four:?}"
        );
        assert!(!turn_four.system.contains(offer_line), "{turn_four:?}");
        assert!(
            !turn_four.current.contains("codemod offer(s)"),
            "no offer notice may reach turn 4, under any id: {turn_four:?}"
        );

        drop(subscription);
        drop(agent);
        drop(host);
        Ok(())
    }
}
