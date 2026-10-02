use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;

use dal_agent::ext::hooks::{Channel, TurnInfo, WatchFactory};
use dal_core::{CallId, SessionId, TurnId};
use sonic_rs::JsonValueTrait;

use super::stream::{Calibration, G8Rule, Sample, SampleSet};
use super::{
    Engine, GuardConfig, Session, TurnState, checks, metrics, report, strike, terse, warnings,
};
use crate::parse::{self, Parsed};
use crate::patch::{
    DiffHunk, DiffLine, DiffLineKind, EditObserver, FindingSeverity, StagedBatch, StagedFile,
};
fn test_config() -> GuardConfig {
    GuardConfig {
        enabled: true,
        cognitive_band: 15,
        cyclomatic_band: 15,
        function_ploc_band: 50,
        nesting_band: 4,
        file_ploc_band: 500,
        guard_wrap: true,
        broad_handler: true,
        helper: true,
        cannot_block: Vec::new(),
        calibrated: BTreeSet::new(),
        erosion_threshold: 0.05,
        churn_threshold: 3,
    }
}

fn session_id() -> SessionId {
    SessionId::new_v7()
}

fn turn_id() -> TurnId {
    TurnId::new(NonZeroU64::MIN)
}

fn engine_with_turn(cfg: GuardConfig) -> (Arc<Engine>, SessionId, TurnId) {
    let engine = Arc::new(Engine::new(cfg));
    let session = session_id();
    let turn = turn_id();
    let mut state = engine.state.lock().expect("engine state");
    state.sessions.insert(
        session,
        Session {
            reset_due: false,
            seen_warnings: std::collections::HashSet::default(),
            turn: Some(TurnState {
                id: turn,
                reduction_ask: false,
                counted: std::collections::HashSet::default(),
                added: 0,
                deleted: 0,
                files: BTreeSet::new(),
                new_files: BTreeSet::new(),
                deletions: BTreeMap::new(),
                churn: BTreeMap::new(),
                strikes: strike::Stream::default(),
                last_error: None,
                pending_notices: Vec::new(),
                sequence: 0,
                calls: std::collections::HashMap::default(),
                first_pre: BTreeMap::new(),
                last_post: BTreeMap::new(),
                bands: Vec::new(),
                warnings: Vec::new(),
                stream_counts: BTreeMap::new(),
                fired: BTreeSet::new(),
                findings: BTreeMap::new(),
                notices: std::collections::HashSet::default(),
            }),
            pending: None,
            announced_blocks: false,
            last: None,
        },
    );
    state.turns.insert(turn, session);
    drop(state);
    (engine, session, turn)
}

async fn parsed(path: &str, src: &str) -> Arc<Parsed> {
    Arc::new(
        parse::tree(Path::new(path), src.as_bytes())
            .await
            .expect("parse fixture"),
    )
}

fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

#[test]
fn placeholder_reject() {
    let rejection = checks::placeholder("...\n}").expect("placeholder rejects");
    assert_eq!(rejection.0, report::PLACEHOLDER_REJECT);
}

#[test]
fn placeholder_non_reject() {
    assert!(checks::placeholder("fn f(x: &[i32]) -> &[i32] { &x[1..] }").is_none());
    assert!(checks::placeholder("const ys = [...xs];").is_none());
    assert!(checks::placeholder("").is_none());
}

#[test]
fn json_strict_gate() {
    let outcome = checks::parse_gate(
        "x.json",
        Path::new("/ws/x.json"),
        None,
        b"{\"a\": 1,}",
        None,
        None,
    );
    match outcome {
        checks::GateOutcome::Reject(rejection) => {
            assert!(rejection.0.contains("Re-read x.json:1"), "{rejection:?}");
        }
        other => panic!("expected reject, got {other:?}"),
    }
    let outcome = checks::parse_gate(
        "x.json",
        Path::new("/ws/x.json"),
        None,
        b"{\"a\": 1}",
        None,
        None,
    );
    assert!(matches!(outcome, checks::GateOutcome::Pass(_)));
}

#[tokio::test]
async fn nat1_exemption() {
    let pre = parsed("/ws/a.rs", "fn broken( {\n").await;
    let post = parsed("/ws/a.rs", "fn still_broken( {\n").await;
    let outcome = checks::parse_gate(
        "a.rs",
        Path::new("/ws/a.rs"),
        Some(b"fn broken( {\n"),
        b"fn still_broken( {\n",
        Some(&pre),
        Some(&post),
    );
    assert!(
        matches!(outcome, checks::GateOutcome::Exempt(_)),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn nat1_rejects_reverts() {
    let pre_src = "let f x = match x with | A -> 1 | B -> 2\n";
    let post_src = "let f x = match x with | A -> 1 | B 2\n";
    let pre = parsed("/ws/a.ml", pre_src).await;
    let post = parsed("/ws/a.ml", post_src).await;
    let outcome = checks::parse_gate(
        "a.ml",
        Path::new("/ws/a.ml"),
        Some(pre_src.as_bytes()),
        post_src.as_bytes(),
        Some(&pre),
        Some(&post),
    );
    match outcome {
        checks::GateOutcome::Reject(rejection) => {
            assert!(rejection.0.contains("a.ml"), "{rejection:?}");
            assert!(rejection.0.starts_with("EDIT REJECTED."), "{rejection:?}");
        }
        other => panic!("expected reject, got {other:?}"),
    }
}

#[tokio::test]
async fn invalid_to_valid_recovery() {
    let pre = parsed("/ws/a.rs", "fn broken( {\n").await;
    let post = parsed("/ws/a.rs", "fn fixed() {}\n").await;
    let outcome = checks::parse_gate(
        "a.rs",
        Path::new("/ws/a.rs"),
        Some(b"fn broken( {\n"),
        b"fn fixed() {}\n",
        Some(&pre),
        Some(&post),
    );
    assert!(
        matches!(
            outcome,
            checks::GateOutcome::Pass(_) | checks::GateOutcome::Exempt(_)
        ),
        "{outcome:?}"
    );
}

#[test]
fn guard_wrap_notice() {
    let hunks = [checks::Hunk {
        removed: vec!["go();".into()],
        added: vec![(1, "if ok {".into()), (2, "go();".into()), (3, "}".into())],
    }];
    let finding = checks::guard_wrap(&hunks).expect("guard wrap fires");
    assert_eq!(finding.rule, super::Rule::GuardWrap);
    assert_eq!(finding.text.as_ref(), report::GUARD_WRAP_NOTICE);
}

#[test]
fn broad_handler_one_per_file() {
    let hunks = [checks::Hunk {
        removed: Vec::new(),
        added: vec![
            (3, "| _ -> 0".into()),
            (4, "| A -> 1".into()),
            (5, "| _ -> 2".into()),
        ],
    }];
    let finding = checks::broad_handler(parse::Language::Ocaml, &hunks, "a.ml")
        .expect("broad handler fires once");
    assert_eq!(
        finding.text.as_ref(),
        &report::broad_handler_notice("a.ml", 3)
    );
}

#[tokio::test]
async fn helper_hint() {
    let pre_src = "fn main() {\n}\n";
    let post_src = "fn h() -> i32 {\n 1\n}\nfn main() {\n let _ = h();\n}\n";
    let pre = parsed("/ws/src/a.rs", pre_src).await;
    let post = parsed("/ws/src/a.rs", post_src).await;
    let pre_metrics = metrics::measure(parse::Language::Rust, &pre.tree, pre_src.as_bytes());
    let post_metrics = metrics::measure(parse::Language::Rust, &post.tree, post_src.as_bytes());
    let findings = checks::helper(
        parse::Language::Rust,
        "src/a.rs",
        Some(&pre_metrics),
        &post,
        post_src.as_bytes(),
        &post_metrics,
    );
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].text.as_ref(), report::HELPER_NOTICE);
}

#[tokio::test]
async fn helper_skips_tests_and_reuse() {
    let pre_src = "fn main() {\n}\n";
    let post_src = "fn h() -> i32 {\n 1\n}\nfn main() {\n let _ = h();\n}\n";
    let pre = parsed("/ws/src/tests/a.rs", pre_src).await;
    let post = parsed("/ws/src/tests/a.rs", post_src).await;
    let pre_metrics = metrics::measure(parse::Language::Rust, &pre.tree, pre_src.as_bytes());
    let post_metrics = metrics::measure(parse::Language::Rust, &post.tree, post_src.as_bytes());
    assert!(
        checks::helper(
            parse::Language::Rust,
            "src/tests/a.rs",
            Some(&pre_metrics),
            &post,
            post_src.as_bytes(),
            &post_metrics,
        )
        .is_empty()
    );
    let post_twice_src = "fn h() -> i32 {\n 1\n}\nfn main() {\n let _ = h();\n let _ = h();\n}\n";
    let post_twice = parsed("/ws/src/a.rs", post_twice_src).await;
    let post_twice_metrics = metrics::measure(
        parse::Language::Rust,
        &post_twice.tree,
        post_twice_src.as_bytes(),
    );
    assert!(
        checks::helper(
            parse::Language::Rust,
            "src/a.rs",
            Some(&pre_metrics),
            &post_twice,
            post_twice_src.as_bytes(),
            &post_twice_metrics,
        )
        .is_empty()
    );
}

#[tokio::test]
async fn commented_out_code() {
    let post_src = "fn main() {\n // let stale = 1;\n let live = 2;\n}\n";
    let post = parsed("/ws/a.rs", post_src).await;
    let added: BTreeSet<u32> = [2].into_iter().collect();
    let findings = checks::commented_out(parse::Language::Rust, &post, post_src.as_bytes(), &added);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].whole_line);
    assert_eq!(findings[0].rule, super::Rule::CommentedOutCode);
}

#[test]
fn bypass_comments() {
    let src = "// guard-disable-file(broad_handler)\nlet x = 1; // guard-allow(helper)\n";
    assert!(checks::bypassed(super::Rule::BroadHandler, src, 2));
    assert!(checks::bypassed(super::Rule::Helper, src, 2));
    assert!(!checks::bypassed(super::Rule::GuardWrap, src, 2));
    let all = "// guard-disable-file(all)\nlet x = 1;\n";
    assert!(checks::bypassed(super::Rule::GuardWrap, all, 2));
}

#[tokio::test]
async fn metrics_rust_golden() {
    let src = "fn f(x: i32) -> i32 {\n if x > 0 {\n x + 1\n } else {\n x - 1\n }\n}\n";
    let post = parsed("/ws/a.rs", src).await;
    let measured = metrics::measure(parse::Language::Rust, &post.tree, src.as_bytes());
    assert_eq!(measured.functions.len(), 1);
    let function = &measured.functions[0];
    assert_eq!(function.name.as_ref(), "f");
    assert_eq!((function.start_line, function.end_line), (1, 7));
    assert_eq!(function.cyclomatic, 2);
    assert_eq!(function.cognitive, 1);
    assert_eq!(function.nesting, 1);
    assert_eq!(function.ploc, 7);
    assert_eq!(measured.cog_sum, 1);
    assert_eq!(measured.cc_sum, 2);
}

#[test]
fn delta_only_band_crossing() {
    let function = |cognitive: u32| super::FunctionMetrics {
        name: "f".into(),
        start_line: 1,
        end_line: 10,
        cognitive,
        cyclomatic: 2,
        ploc: 10,
        nesting: 1,
    };
    let pre = super::FileMetrics {
        ploc: 10,
        functions: vec![function(9)],
        cog_sum: 9,
        cc_sum: 2,
    };
    let post_ten = super::FileMetrics {
        ploc: 10,
        functions: vec![function(10)],
        cog_sum: 10,
        cc_sum: 2,
    };
    assert!(metrics::crossings("a.rs", Some(&pre), &post_ten, 15, 15, 50, 4, 500).is_empty());
    let post_big = super::FileMetrics {
        ploc: 10,
        functions: vec![function(17)],
        cog_sum: 17,
        cc_sum: 2,
    };
    let crossings = metrics::crossings("a.rs", Some(&pre), &post_big, 15, 15, 50, 4, 500);
    assert_eq!(crossings.len(), 1, "{crossings:?}");
    assert_eq!(crossings[0].line, "f f cognitive 9→17 (over 15)");
}

#[test]
fn erosion_mass_fixture() {
    let function = |cyclomatic: u32, ploc: u32| super::FunctionMetrics {
        name: "f".into(),
        start_line: 1,
        end_line: 2,
        cognitive: 0,
        cyclomatic,
        ploc,
        nesting: 0,
    };
    let functions = [function(10, 196), function(14, 25)];
    let erosion = metrics::erosion(functions.iter());
    assert!((erosion - 20.0 / 60.0).abs() < 1e-9, "{erosion}");
    assert_eq!(
        report::metrics_line(1, 0, 0.0, erosion, true),
        format!(
            "METRICS: files 1, bands crossed 0, erosion 0.00→0.33{}",
            report::HUMAN_BASELINE
        )
    );
}

#[test]
fn ledger_numbers() {
    let summary = report::TurnSummary {
        added: 10,
        deleted: 7,
        files: vec!["a.rs".into(), "b.rs".into()],
        new_files: 0,
        reduction_ask: false,
        pure_additions: Vec::new(),
        announce_reset: false,
        bands: Vec::new(),
        mass_rows: Vec::new(),
        erosion: None,
        erosion_threshold: 0.05,
        churn: Vec::new(),
        warnings: Vec::new(),
        stream: Vec::new(),
    };
    let text = report::turn_report(&summary).expect("ledger report");
    assert!(
        text.starts_with("TURN CHANGE SUMMARY. Added 10, deleted 7, net 3, files 2, new files 0."),
        "{text}"
    );
}

#[test]
fn reduction_share_line() {
    assert_eq!(report::deletion_share(40.0 / 45.0), "deletion share 0.89");
}

#[test]
fn silence_when_healthy() {
    let summary = report::TurnSummary {
        added: 3,
        deleted: 1,
        files: vec!["a.rs".into()],
        new_files: 0,
        reduction_ask: false,
        pure_additions: Vec::new(),
        announce_reset: false,
        bands: Vec::new(),
        mass_rows: Vec::new(),
        erosion: Some((0.0, 0.0)),
        erosion_threshold: 0.05,
        churn: Vec::new(),
        warnings: Vec::new(),
        stream: Vec::new(),
    };
    let text = report::turn_report(&summary).expect("healthy report");
    assert!(!text.contains("METRICS"), "{text}");
    assert!(!text.contains("NOTICE"), "{text}");
}

#[test]
fn churn_lines_in_report() {
    let summary = report::TurnSummary {
        added: 1,
        deleted: 0,
        files: vec!["a.rs".into()],
        new_files: 0,
        reduction_ask: false,
        pure_additions: Vec::new(),
        announce_reset: false,
        bands: Vec::new(),
        mass_rows: Vec::new(),
        erosion: None,
        erosion_threshold: 0.05,
        churn: vec![("a.rs".into(), 3)],
        warnings: Vec::new(),
        stream: Vec::new(),
    };
    let text = report::turn_report(&summary).expect("churn report");
    assert!(
        text.contains("churn a.rs touched 3 times this turn"),
        "{text}"
    );
}

#[test]
fn strike_canonical_rounding() {
    let left: sonic_rs::Value = sonic_rs::from_str("{\"n\": 1.234}").expect("json");
    let right: sonic_rs::Value = sonic_rs::from_str("{\"n\": 1.2344}").expect("json");
    let far: sonic_rs::Value = sonic_rs::from_str("{\"n\": 1.244}").expect("json");
    assert_eq!(strike::canonical(&left), strike::canonical(&right));
    assert_ne!(strike::canonical(&left), strike::canonical(&far));
    assert!((strike::round3(1.234) - 1.23).abs() < f64::EPSILON);
    assert!(strike::round3(0.0).abs() < f64::EPSILON);
    let key = strike::call_key("patch", &left);
    assert_eq!(
        strike::display_key("patch", &key).len(),
        "patch ".len() + 12
    );
}

#[test]
fn strike_report_only_never_blocks() {
    let args: sonic_rs::Value = sonic_rs::from_str("{\"path\": \"a.rs\"}").expect("json");
    let key = strike::call_key("patch", &args);
    let mut stream = strike::Stream::default();
    assert_eq!(stream.on_call(key, 1), 1);
    stream.on_result(key, 1, true);
    assert_eq!(stream.on_call(key, 2), 2);
    stream.on_result(key, 2, false);
    assert_eq!(stream.on_call(key, 3), 3);
    assert_eq!(stream.strikes(), 3);
    stream.on_result(key, 99, false);
    assert_eq!(stream.strikes(), 3);
}

#[test]
fn warnings_scan_rust_and_ocaml() {
    let rust = "warning: unused variable: `x`\n   --> src/a.rs:12:9\n";
    let found = warnings::scan(rust);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!((found[0].path.as_ref(), found[0].line), ("src/a.rs", 12));
    let ocaml = "File \"a.ml\", line 3, characters 4-5:\nWarning 26: unused variable x.\n";
    let found = warnings::scan(ocaml);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!((found[0].path.as_ref(), found[0].line), ("a.ml", 3));
    assert!(warnings::grep_like("grep foo"));
    assert!(warnings::grep_like("git grep foo"));
    assert!(!warnings::grep_like("cargo build"));
    assert_eq!(
        warnings::full_output_path("done\nFull output: /tmp/jobs/c1.log\n"),
        Some("/tmp/jobs/c1.log")
    );
}

#[test]
fn calibration_gate() {
    assert!(!Calibration::none().admits(G8Rule::DebugPrint));
    let admitted = Calibration {
        sets: [(
            G8Rule::BareTodo,
            SampleSet {
                samples: (0..50)
                    .map(|index| Sample {
                        text: format!("// TODO {index}").into(),
                        positive: index < 48,
                    })
                    .collect(),
            },
        )]
        .into_iter()
        .collect(),
    };
    assert!(admitted.admits(G8Rule::BareTodo));
    let thin = Calibration {
        sets: [(
            G8Rule::BareTodo,
            SampleSet {
                samples: (0..50)
                    .map(|index| Sample {
                        text: format!("// TODO {index}").into(),
                        positive: index < 47,
                    })
                    .collect(),
            },
        )]
        .into_iter()
        .collect(),
    };
    assert!(!thin.admits(G8Rule::BareTodo));
    assert_eq!(G8Rule::parse("vibes"), None);
    assert_eq!(G8Rule::parse("bare_todo"), Some(G8Rule::BareTodo));
}

#[tokio::test]
async fn stream_report_only_before_calibration() {
    let (engine, session, turn) = engine_with_turn(test_config());
    let router = super::stream::Router::new(engine);
    let mut watch = router
        .start(&TurnInfo::new(session, turn))
        .expect("watch starts");
    let name: dal_core::Name = "patch".parse().expect("tool name");
    let verdict = watch.feed(
        Channel::ToolArgs { tool: name },
        "{\"new_text\":\"// TODO\"}",
    );
    assert!(matches!(verdict, dal_core::StreamVerdict::Continue));
    let verdict = watch.finish();
    assert!(matches!(verdict, dal_core::StreamVerdict::Continue));
}

#[tokio::test]
async fn stream_interrupt_after_calibration() {
    let mut cfg = test_config();
    cfg.calibrated.insert(G8Rule::BareTodo);
    let (engine, session, turn) = engine_with_turn(cfg);
    let router = super::stream::Router::new(engine);
    let mut watch = router
        .start(&TurnInfo::new(session, turn))
        .expect("watch starts");
    let name: dal_core::Name = "patch".parse().expect("tool name");
    let verdict = watch.feed(
        Channel::ToolArgs { tool: name.clone() },
        "{\"new_text\":\"// TODO\"}\n",
    );
    match verdict {
        dal_core::StreamVerdict::Interrupt { rule, inject } => {
            assert_eq!(rule.as_ref(), "bare_todo");
            assert_eq!(inject.as_ref(), &report::stream_interrupt("bare_todo"));
        }
        other => panic!("expected interrupt, got {other:?}"),
    }
    let verdict = watch.feed(
        Channel::ToolArgs { tool: name },
        "{\"new_text\":\"// TODO\"}\n",
    );
    assert!(matches!(verdict, dal_core::StreamVerdict::Continue));
}

#[test]
fn terse_rewrite_and_safety() {
    assert_eq!(
        terse::terse("Be thorough and consider edge cases. Creates a file."),
        "Creates a file."
    );
    assert_eq!(
        terse::terse("Never deletes files without approval."),
        "Never deletes files without approval."
    );
    let tools = vec![dal_agent::ext::ToolDescription {
        name: "patch".parse().expect("tool name"),
        description: "Be thorough. Applies an edit.".into(),
    }];
    let section = terse::section(&tools);
    assert!(section.starts_with(report::MINIMALISM_RULE), "{section}");
    assert!(section.contains("- patch: Applies an edit."), "{section}");
    assert!(section.ends_with(report::CONTRACTS_FOOTER), "{section}");
}

#[test]
fn dal_default_off() {
    let engine = Engine::new(GuardConfig::disabled());
    let batch = StagedBatch {
        session: session_id(),
        turn: turn_id(),
        call: CallId::new("c1"),
        files: Vec::new(),
    };
    assert!(engine.inspect(&batch).is_empty());
    let router = super::stream::Router::new(Arc::new(Engine::new(GuardConfig::disabled())));
    assert!(
        router
            .start(&TurnInfo::new(SessionId::new_v7(), turn_id()))
            .is_none()
    );
}

#[tokio::test]
async fn inspect_ledger_and_receipt() {
    let (engine, session, turn) = engine_with_turn(test_config());
    let before = "fn main() {\n}\n";
    let after = "fn main() {\n let x = 1;\n}\n";
    let pre = parsed("/ws/a.rs", before).await;
    let post = parsed("/ws/a.rs", after).await;
    let hunks = [DiffHunk {
        old_start: 1,
        old_lines: 2,
        new_start: 1,
        new_lines: 3,
        lines: vec![
            DiffLine {
                kind: DiffLineKind::Context,
                text: "fn main() {".into(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                text: " let x = 1;".into(),
            },
            DiffLine {
                kind: DiffLineKind::Context,
                text: "}".into(),
            },
        ],
    }];
    let files = [StagedFile {
        path: Path::new("a.rs"),
        absolute_path: Path::new("/ws/a.rs"),
        before: Some(before.as_bytes()),
        after: Some(after.as_bytes()),
        hunks: &hunks,
        pre_parse: Some(pre),
        post_parse: Some(post),
    }];
    let batch = StagedBatch {
        session,
        turn,
        call: CallId::new("c1"),
        files: files.to_vec(),
    };
    let out = engine.inspect(&batch);
    assert!(
        out.iter()
            .any(|finding| finding.severity == FindingSeverity::Report
                && finding.text.starts_with("guard: a.rs ploc ")),
        "{out:?}"
    );
    assert!(
        !out.iter()
            .any(|finding| finding.severity == FindingSeverity::Block),
        "{out:?}"
    );
    let state = engine.state.lock().expect("engine state");
    let session = state.sessions.get(&session).expect("session");
    let turn = session.turn.as_ref().expect("turn");
    assert_eq!((turn.added, turn.deleted), (1, 0));
    assert!(turn.files.contains("a.rs"));
}

#[tokio::test]
async fn inspect_counts_ledger_without_parses() {
    let (engine, session, turn) = engine_with_turn(test_config());
    let a_hunks = [DiffHunk {
        old_start: 1,
        old_lines: 2,
        new_start: 1,
        new_lines: 10,
        lines: (0..10)
            .map(|_| DiffLine {
                kind: DiffLineKind::Added,
                text: "x".into(),
            })
            .chain((0..2).map(|_| DiffLine {
                kind: DiffLineKind::Removed,
                text: "y".into(),
            }))
            .collect(),
    }];
    let b_hunks = [DiffHunk {
        old_start: 1,
        old_lines: 5,
        new_start: 1,
        new_lines: 0,
        lines: (0..5)
            .map(|_| DiffLine {
                kind: DiffLineKind::Removed,
                text: "y".into(),
            })
            .collect(),
    }];
    let files = [
        StagedFile {
            path: Path::new("a.txt"),
            absolute_path: Path::new("/ws/a.txt"),
            before: Some(b"old".as_ref()),
            after: Some(b"new"),
            hunks: &a_hunks,
            pre_parse: None,
            post_parse: None,
        },
        StagedFile {
            path: Path::new("b.txt"),
            absolute_path: Path::new("/ws/b.txt"),
            before: Some(b"old".as_ref()),
            after: Some(b"new"),
            hunks: &b_hunks,
            pre_parse: None,
            post_parse: None,
        },
    ];
    let batch = StagedBatch {
        session,
        turn,
        call: CallId::new("c1"),
        files: files.to_vec(),
    };
    let out = engine.inspect(&batch);
    assert!(out.is_empty(), "{out:?}");
    let text = {
        let state = engine.state.lock().expect("engine state");
        let session = state.sessions.get(&session).expect("session");
        let turn = session.turn.as_ref().expect("turn");
        assert_eq!((turn.added, turn.deleted), (10, 7));
        assert_eq!(turn.files.len(), 2);
        let summary = report::TurnSummary {
            added: turn.added,
            deleted: turn.deleted,
            files: turn.files.iter().cloned().collect(),
            new_files: turn.new_files.len(),
            reduction_ask: false,
            pure_additions: Vec::new(),
            announce_reset: false,
            bands: Vec::new(),
            mass_rows: Vec::new(),
            erosion: None,
            erosion_threshold: 0.05,
            churn: Vec::new(),
            warnings: Vec::new(),
            stream: Vec::new(),
        };
        report::turn_report(&summary).expect("ledger text")
    };
    assert!(
        text.starts_with("TURN CHANGE SUMMARY. Added 10, deleted 7, net 3, files 2, new files 0."),
        "{text}"
    );
}

#[test]
fn findings_json_shape() {
    let findings = super::GuardFindings {
        turn: turn_id(),
        files: vec![super::FileFindings {
            path: "src/a.rs".into(),
            verdict: super::Verdict::Findings,
            items: vec![
                super::Finding {
                    rule: super::Rule::BroadHandler,
                    line: 7,
                    line_end: 7,
                    whole_line: false,
                    text: "notice".into(),
                },
                super::Finding {
                    rule: super::Rule::GuardWrap,
                    line: 3,
                    line_end: 3,
                    whole_line: false,
                    text: "wrap".into(),
                },
            ],
            metrics: None,
        }],
        warnings: Vec::new(),
        stream: Vec::new(),
        strikes: 0,
        report: None,
    };
    let json = super::findings_json(&findings);
    let value: sonic_rs::Value = sonic_rs::from_str(&json).expect("findings json");
    let first = value.get("findings").expect("findings array");
    assert_eq!(
        first
            .get(0)
            .expect("entry")
            .get("rule")
            .expect("rule")
            .as_str(),
        Some("broad_handler")
    );
    assert_eq!(
        first
            .get(0)
            .expect("entry")
            .get("line_end")
            .expect("span")
            .as_u64(),
        Some(7)
    );
}

#[test]
fn strike_and_report_text_contracts() {
    assert_eq!(
        report::strike_notice(1, "patch returned an error", "syntax", "a.rs"),
        "<guard strike=\"1/3\">This call failed: patch returned an error. Cause: syntax. Evidence: a.rs. Next: re-read that region and submit one different edit. Do not repeat this call verbatim.</guard>"
    );
    assert_eq!(
        report::exhaustion("patch abc123", "none", "a.rs"),
        "Guard stopped this turn after 3 strikes. No pending retry was run. Repeated target: patch abc123. Last failure: none. Persistent changes in this turn: a.rs."
    );
    assert_eq!(
        report::TURN_DENIED,
        "guard: turn service denied; strike stop-downgrade active"
    );
    assert_eq!(
        report::FS_READ_DENIED,
        "guard: fs.read denied; warning router disabled this session"
    );
}

#[test]
fn ledger_arithmetic_property() {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    for _ in 0..1000 {
        let added = xorshift(&mut state) % 50;
        let deleted = xorshift(&mut state) % 50;
        let files = usize::try_from(xorshift(&mut state) % 5).unwrap_or(0);
        let new_files =
            usize::try_from(xorshift(&mut state) % (u64::try_from(files).unwrap_or(0) + 1))
                .unwrap_or(0);
        let line = report::ledger(added, deleted, files, new_files);
        let net = i128::from(added) - i128::from(deleted);
        assert!(
            line.contains(&format!("Added {added}, deleted {deleted}, net {net}")),
            "{line}"
        );
        assert!(
            line.contains(&format!("files {files}, new files {new_files}")),
            "{line}"
        );
    }
}

#[test]
fn hunk_count_property() {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    for _ in 0..1000 {
        let count = (xorshift(&mut state) % 12) as usize;
        let mut lines = Vec::new();
        let mut expected_added = 0_u64;
        let mut expected_deleted = 0_u64;
        for _ in 0..count {
            let kind = match xorshift(&mut state) % 3 {
                0 => {
                    expected_added += 1;
                    DiffLineKind::Added
                }
                1 => {
                    expected_deleted += 1;
                    DiffLineKind::Removed
                }
                _ => DiffLineKind::Context,
            };
            lines.push(DiffLine {
                kind,
                text: "x".into(),
            });
        }
        let hunks = [DiffHunk {
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 3,
            lines,
        }];
        let (added, deleted) = super::observe::hunk_counts(&hunks);
        assert_eq!((added, deleted), (expected_added, expected_deleted));
    }
}

#[test]
fn strike_counter_property() {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    for _ in 0..1000 {
        let mut stream = strike::Stream::default();
        let mut last = None;
        let mut expected = 0_u8;
        let mut errored = false;
        let steps = (xorshift(&mut state) % 8) + 1;
        for sequence in 1..=steps {
            let key = [(xorshift(&mut state) % 4) as u8; 16];
            let ok = xorshift(&mut state).is_multiple_of(2);
            let got = stream.on_call(key, u128::from(sequence));
            if last == Some(key) {
                expected = expected.saturating_add(if errored { 2 } else { 1 }).min(3);
            } else {
                last = Some(key);
                expected = 1;
            }
            assert_eq!(got, expected, "sequence {sequence}");
            stream.on_result(key, u128::from(sequence), ok);
            errored = !ok;
            assert!(stream.strikes() <= 3);
        }
    }
}

#[tokio::test]
async fn metric_purity_bounds() {
    let fragments = [
        "if x > 0 {\n x\n}\n",
        "while y {\n y\n}\n",
        "match z {\n _ => 1,\n}\n",
        "a && b\n",
        "if a {\n if b {\n c\n}\n}\n",
    ];
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    for _ in 0..200 {
        let mut body = String::from("fn f() {\n");
        for _ in 0..=(xorshift(&mut state) % 3) {
            body.push_str(
                fragments
                    [usize::try_from(xorshift(&mut state) % fragments.len() as u64).unwrap_or(0)],
            );
        }
        body.push_str("}\n");
        let Ok(parsed) = parse::tree(Path::new("/ws/f.rs"), body.as_bytes()).await else {
            continue;
        };
        let first = metrics::measure(parse::Language::Rust, &parsed.tree, body.as_bytes());
        let second = metrics::measure(parse::Language::Rust, &parsed.tree, body.as_bytes());
        assert_eq!(first, second);
        for function in &first.functions {
            assert!(function.cyclomatic >= 1);
        }
        let erosion = metrics::erosion(first.functions.iter());
        assert!((0.0..=1.0).contains(&erosion), "{erosion}");
    }
}
