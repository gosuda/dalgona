use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use dal_agent::error::ServiceError;
use dal_agent::ext::hooks::{Channel, TurnInfo, WatchFactory};
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{
    Caller, Doc, EventStream, Hook, HookCx, HookError, ObserveHook, RawValue, Services, Tool,
    ToolOutcome,
};
use dal_core::ext::{
    BeforeTurn, McpDeclaration, McpRequest, McpResponse, SessionEnd, SessionStart, StateError,
    StateOp, StateRecord, ToolCallEvent, ToolResultEvent, TurnEnd, Visibility,
};
use dal_core::{
    AgentsOp, AgentsReply, Answer, CallId, DenyReason, EntryId, FetchRequest, FetchResponse,
    Inference, JobsOp, JobsReply, ModelRequest, Name, Notice, Question, RawJson, RunOutput,
    RunRequest, SessionId, SidecarOp, Stop, ToolCallVerdict, ToolClass, TurnId, TurnOp,
    TurnOpReply, Workspace,
};
use sonic_rs::JsonValueTrait;
use tokio::sync::Notify;

use super::hooks::{
    BeforeTurnHook, SessionEndHook, SessionStartHook, ToolCallHook, ToolResultHook, TurnEndHook,
};
use super::stream::{Calibration, G8Rule, Sample, SampleSet};
use super::{
    Engine, FindingsHandle, GuardConfig, GuardConfigError, GuardFindings, Session, TurnState,
    checks, metrics, report, strike, strike_key, terse, warnings,
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
    let mut seeded = Session::new(false);
    seeded.turn = Some(TurnState::new(turn, 1, false));
    state.last_epoch = 1;
    state.sessions.insert(session, seeded);
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

/// Builds the guard bands fixture used by the crossing tests.
fn bands(
    cognitive: u32,
    cyclomatic: u32,
    function_ploc: u32,
    nesting: u32,
    file_ploc: u32,
) -> metrics::Bands {
    metrics::Bands {
        cognitive,
        cyclomatic,
        function_ploc,
        nesting,
        file_ploc,
    }
}

#[test]
fn placeholder_reject() {
    let rejection = checks::placeholder("...\n}").expect("placeholder rejects");
    assert!(rejection.0.starts_with("PATCH REJECTED."), "{rejection:?}");
    assert!(
        rejection
            .0
            .contains("Write the complete replacement text; nothing changed."),
        "{rejection:?}"
    );
}

#[test]
fn placeholder_non_reject() {
    assert!(checks::placeholder("fn f(x: &[i32]) -> &[i32] { &x[1..] }").is_none());
    assert!(checks::placeholder("const ys = [...xs];").is_none());
    assert!(checks::placeholder("").is_none());
}

/// Builds a placeholder fixture from a template: `~` stands for the
/// three-dot ellipsis and `^` for the single-character ellipsis.
fn ellipsis(template: &str) -> String {
    template
        .replace('~', "\u{2e}\u{2e}\u{2e}")
        .replace('^', "\u{2026}")
}

const NINETEEN: &str = "abcdefghijklmnopqrs";
const TWENTY: &str = "abcdefghijklmnopqrst";

#[test]
fn placeholder_rejects_every_ellipsis_only_form() {
    for template in [
        "~",
        "^",
        "// ~",
        "// ^",
        "# ~",
        "-- ~",
        "/* ~ */",
        "(* ~ *)",
        "//~",
        "#^",
        "/*~*/",
        "    ~",
        "\t^  ",
        "~\r\n}",
        "{\n    ~\n}",
        "~\n~\n~",
    ] {
        let added = ellipsis(template);
        let Some(rejection) = checks::placeholder(&added) else {
            panic!("{added:?} must be rejected");
        };
        assert!(
            rejection.0.starts_with("PATCH REJECTED."),
            "{added:?}: {rejection:?}"
        );
    }
}

#[test]
fn placeholder_ignores_ellipsis_that_is_code_or_prose() {
    for template in [
        "~xs",
        "[~xs]",
        "const ys = [~xs];",
        "{ ~rest }",
        "f(~args)",
        "let s = \"~\";",
        "x = ~",
        "def f(): ~",
        "1~5",
        "a ~ b",
        "wait^",
        "/* ~",
        "(* ~",
        "..",
        "&x[1..]",
    ] {
        let added = ellipsis(template);
        assert!(
            checks::placeholder(&added).is_none(),
            "{added:?} is code or prose, not a placeholder"
        );
    }
}

#[test]
fn placeholder_flips_at_twenty_non_whitespace_characters_beside_the_ellipsis() {
    assert_eq!(NINETEEN.chars().count(), 19);
    assert_eq!(TWENTY.chars().count(), 20);
    let spaced = |body: &str| {
        body.chars()
            .map(String::from)
            .collect::<Vec<_>>()
            .join(" \t")
    };
    for marker in ["~", "^", "// ~", "# ^", "/* ~ */", "(* ~ *)"] {
        let marker = ellipsis(marker);
        for (body, rejected) in [
            (NINETEEN.to_owned(), true),
            (TWENTY.to_owned(), false),
            (spaced(NINETEEN), true),
            (spaced(TWENTY), false),
        ] {
            for added in [
                format!("{marker}\n{body}"),
                format!("{body}\n{marker}"),
                format!("{marker}\n{marker}\n{body}\n{marker}"),
            ] {
                assert_eq!(checks::placeholder(&added).is_some(), rejected, "{added:?}");
            }
        }
    }
}

#[test]
fn placeholder_phrases_reject_and_stay_out_of_the_character_count() {
    for phrase in [
        "rest of methods",
        "implementation omitted",
        "// ~ rest of the code",
        "# ~ rest of implementation",
        "-- ~ rest of implementation",
        "(* ~ rest of implementation *)",
    ] {
        let phrase = ellipsis(phrase);
        assert!(checks::placeholder(&phrase).is_some(), "{phrase:?} alone");
        for (body, rejected) in [(NINETEEN, true), (TWENTY, false)] {
            for added in [
                format!("{body}\n{phrase}"),
                format!("{body} {phrase}"),
                format!("{phrase}\n{body}"),
                format!("{body} {phrase} {phrase}"),
            ] {
                assert_eq!(checks::placeholder(&added).is_some(), rejected, "{added:?}");
            }
        }
    }
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
async fn parse_gate_pins_the_one_based_position_of_a_missing_token() {
    // The position is where the grammar inserted the missing token, counted
    // from one on both axes.
    for (path, src, token, line, column) in [
        ("a.rs", "fn main() {\n    let x = 1\n}\n", ";", 2, 14),
        ("a.rs", "fn main() {\n    foo(1, 2;\n}\n", ")", 2, 13),
        ("a.rs", "struct S {\n    a: u32,\n    b: u32\n", "}", 3, 11),
        ("a.js", "const a = [1, 2;\n", "]", 1, 16),
        ("a.c", "int main(void) {\n  return 0\n}\n", ";", 2, 11),
    ] {
        let post = parsed(&format!("/ws/{path}"), src).await;
        let outcome = checks::parse_gate(
            path,
            Path::new(&format!("/ws/{path}")),
            None,
            src.as_bytes(),
            None,
            Some(&post),
        );
        let checks::GateOutcome::Reject(rejection) = outcome else {
            panic!("{path} {src:?} must be rejected, got {outcome:?}");
        };
        assert!(rejection.0.starts_with("EDIT REJECTED."), "{rejection:?}");
        let cause = format!(
            "Cause: missing {token} at line {line}, column {column}. Re-read {path}:{line},"
        );
        assert!(rejection.0.contains(&cause), "{cause} in {rejection:?}");
    }
}

#[tokio::test]
async fn parse_gate_pins_the_one_based_position_of_a_syntax_error() {
    for (path, src, line, column) in [("a.rs", "fn main() {\n    let x = ;\n}\n", 2, 11)] {
        let post = parsed(&format!("/ws/{path}"), src).await;
        let outcome = checks::parse_gate(
            path,
            Path::new(&format!("/ws/{path}")),
            None,
            src.as_bytes(),
            None,
            Some(&post),
        );
        let checks::GateOutcome::Reject(rejection) = outcome else {
            panic!("{path} {src:?} must be rejected, got {outcome:?}");
        };
        let cause =
            format!("Cause: syntax error at line {line}, column {column}. Re-read {path}:{line},");
        assert!(rejection.0.contains(&cause), "{cause} in {rejection:?}");
    }
}

#[test]
fn parse_gate_reports_the_json_error_line_not_line_one() {
    for (body, line) in [
        ("{\n  \"a\": 1,\n  \"b\": [1, 2\n}\n", 4),
        ("{\"a\":1}\n{\"b\":2}", 2),
        ("{\"a\": 1,}", 1),
    ] {
        let outcome = checks::parse_gate(
            "x.json",
            Path::new("/ws/x.json"),
            None,
            body.as_bytes(),
            None,
            None,
        );
        let checks::GateOutcome::Reject(rejection) = outcome else {
            panic!("{body:?} must be rejected, got {outcome:?}");
        };
        assert!(
            rejection.0.contains(&format!(" at line {line} column ")),
            "{body:?}: {rejection:?}"
        );
        assert!(
            rejection.0.contains(&format!("Re-read x.json:{line},")),
            "{body:?}: {rejection:?}"
        );
    }
}

#[tokio::test]
async fn parse_gate_skips_what_it_cannot_judge_instead_of_rejecting() {
    let broken_rs = parsed("/ws/a.rs", "fn bad( {\n").await;
    // An unguarded file type is never judged, whatever it holds.
    let outcome = checks::parse_gate(
        "notes.txt",
        Path::new("/ws/notes.txt"),
        None,
        b"fn bad( {\n",
        None,
        Some(&broken_rs),
    );
    assert!(
        matches!(outcome, checks::GateOutcome::Skipped),
        "{outcome:?}"
    );
    // Bytes that are not UTF-8 are skipped even when a tree exists.
    let outcome = checks::parse_gate(
        "a.rs",
        Path::new("/ws/a.rs"),
        None,
        b"fn bad( {\xff\n",
        None,
        Some(&broken_rs),
    );
    assert!(
        matches!(outcome, checks::GateOutcome::Skipped),
        "{outcome:?}"
    );
    // A tree built with another grammar proves nothing about this path.
    let python = parsed("/ws/a.py", "x = (\n").await;
    let outcome = checks::parse_gate(
        "a.rs",
        Path::new("/ws/a.rs"),
        None,
        b"x = (\n",
        None,
        Some(&python),
    );
    assert!(
        matches!(outcome, checks::GateOutcome::Skipped),
        "{outcome:?}"
    );
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
    assert_eq!(
        checks::helper(
            parse::Language::Rust,
            "src/tests/a.rs",
            Some(&pre_metrics),
            &post,
            post_src.as_bytes(),
            &post_metrics,
        ),
        []
    );
    let post_twice_src = "fn h() -> i32 {\n 1\n}\nfn main() {\n let _ = h();\n let _ = h();\n}\n";
    let post_twice = parsed("/ws/src/a.rs", post_twice_src).await;
    let post_twice_metrics = metrics::measure(
        parse::Language::Rust,
        &post_twice.tree,
        post_twice_src.as_bytes(),
    );
    assert_eq!(
        checks::helper(
            parse::Language::Rust,
            "src/a.rs",
            Some(&pre_metrics),
            &post_twice,
            post_twice_src.as_bytes(),
            &post_twice_metrics,
        ),
        []
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
    assert!(
        metrics::crossings("a.rs", Some(&pre), &post_ten, &bands(15, 15, 50, 4, 500)).is_empty()
    );
    let post_big = super::FileMetrics {
        ploc: 10,
        functions: vec![function(17)],
        cog_sum: 17,
        cc_sum: 2,
    };
    let crossings = metrics::crossings("a.rs", Some(&pre), &post_big, &bands(15, 15, 50, 4, 500));
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
    assert_eq!(report::deletion_share(0.0), "deletion share 0.00");
    assert_eq!(report::deletion_share(1.0), "deletion share 1.00");
    assert_eq!(report::deletion_share(0.125), "deletion share 0.12");
    assert_eq!(report::deletion_share(f64::NAN), "deletion share NaN");
    assert_eq!(report::deletion_share(f64::INFINITY), "deletion share inf");
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

fn path_key(path: &str) -> [u8; 16] {
    let args: sonic_rs::Value =
        sonic_rs::from_str(&format!("{{\"path\": \"{path}\"}}")).expect("json");
    strike::call_key("patch", &args)
}

#[test]
fn strike_stream_stops_on_the_third_identical_call_and_stays_stopped() {
    let (a, b) = (path_key("a.rs"), path_key("b.rs"));
    let mut stream = strike::Stream::default();
    assert_eq!(stream.on_call(a, 1), strike::Step::Run { strikes: 1 });
    stream.on_result(a, 1, true);
    assert_eq!(stream.on_call(a, 2), strike::Step::Run { strikes: 2 });
    stream.on_result(a, 2, true);
    assert!(!stream.stopped());
    assert_eq!(stream.on_call(a, 3), strike::Step::Stop);
    assert!(stream.stopped());
    assert_eq!(stream.strikes(), 3);
    // Exhaustion is not lifted by a different key, a success, or the repeat.
    assert_eq!(stream.on_call(b, 4), strike::Step::Stop);
    stream.on_result(b, 4, true);
    assert_eq!(stream.on_call(a, 5), strike::Step::Stop);
    assert_eq!(stream.strikes(), 3);
}

#[test]
fn strike_stream_counts_an_errored_repeat_as_two() {
    let a = path_key("a.rs");
    let mut stream = strike::Stream::default();
    assert_eq!(stream.on_call(a, 1), strike::Step::Run { strikes: 1 });
    stream.on_result(a, 1, false);
    assert_eq!(stream.on_call(a, 2), strike::Step::Stop);
}

#[test]
fn strike_stream_ignores_a_result_that_belongs_to_an_earlier_call() {
    let (a, b) = (path_key("a.rs"), path_key("b.rs"));
    let mut stream = strike::Stream::default();
    assert_eq!(stream.on_call(a, 1), strike::Step::Run { strikes: 1 });
    assert_eq!(stream.on_call(b, 2), strike::Step::Run { strikes: 1 });
    assert_eq!(stream.on_call(a, 3), strike::Step::Run { strikes: 1 });
    // The failure of call 1 arrives late; it must not turn call 4 into +2.
    stream.on_result(a, 1, false);
    assert_eq!(stream.on_call(a, 4), strike::Step::Run { strikes: 2 });
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
    // Command output may print the same shape; the tool's own footer is the
    // last such line, and the status lines after it are not footers.
    assert_eq!(
        warnings::full_output_path(
            "Full output: /tmp/jobs/forged.log\nx\nFull output: /tmp/jobs/real.log\nCommand exited with code 1"
        ),
        Some("/tmp/jobs/real.log")
    );
    assert_eq!(warnings::full_output_path("Full output: \nx"), None);
    assert_eq!(warnings::full_output_path("no footer here"), None);
}

#[test]
fn spill_candidates_are_minted_job_id_logs() {
    let minted = format!("/data/sessions/w/s/jobs/{}.log", dal_core::JobId::new_v7());
    let relative = format!("jobs/{}.log", dal_core::JobId::new_v7());
    for path in [minted.as_str(), relative.as_str()] {
        assert!(super::hooks::is_job_log(path), "{path}");
    }
    let id = dal_core::JobId::new_v7().to_string().to_uppercase();
    let uppercase = format!("/s/jobs/{id}.log");
    let not_v7 = "/s/jobs/123e4567-e89b-12d3-a456-426614174000.log";
    for path in [
        "/etc/secret",
        "/s/cache/c1.log",
        "/s/cache/0190aaaa-bbbb-7ccc-8ddd-eeeeffff0000.log",
        "/s/jobs/c1.log",
        "/s/jobs/c1.txt",
        "/s/jobs/other.log",
        "/s/jobs/.log",
        "/s/jobs/",
        "jobs/c1.log",
        "c1.log",
        "",
        uppercase.as_str(),
        not_v7,
    ] {
        assert!(!super::hooks::is_job_log(path), "{path:?}");
    }
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
    assert_eq!(engine.inspect(&batch), []);
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
    // Pin interpolated fields, not boilerplate: every argument must appear
    // verbatim, the strike counter must track n, and a dropped field must
    // turn this test red without any whole-sentence pin.
    let first = report::strike_notice(1, "patch returned an error", "syntax", "a.rs");
    for field in ["1/3", "patch returned an error", "syntax", "a.rs"] {
        assert!(first.contains(field), "{first}");
    }
    assert!(!first.contains("2/3"), "{first}");
    let second = report::strike_notice(2, "patch returned an error", "syntax", "a.rs");
    assert!(second.contains("2/3"), "{second}");
    assert!(!second.contains("1/3"), "{second}");
    let exhausted = report::exhaustion("patch abc123", "none", "a.rs");
    for field in ["patch abc123", "none", "a.rs"] {
        assert!(exhausted.contains(field), "{exhausted}");
    }
}

#[test]
fn denied_notices_name_the_one_grant_that_lifts_them() {
    // The rig tests compare notices against these constants, so only a
    // literal pin notices a reworded or swapped notice.
    assert!(
        report::TURN_DENIED.starts_with("guard: "),
        "{}",
        report::TURN_DENIED
    );
    assert!(
        report::TURN_DENIED.contains("Grant the guard extension the turn service"),
        "{}",
        report::TURN_DENIED
    );
    assert!(
        !report::TURN_DENIED.contains("fs.read"),
        "{}",
        report::TURN_DENIED
    );
    assert!(
        report::FS_READ_DENIED.starts_with("guard: "),
        "{}",
        report::FS_READ_DENIED
    );
    assert!(
        report::FS_READ_DENIED.contains("Grant the guard extension fs.read"),
        "{}",
        report::FS_READ_DENIED
    );
    assert!(
        !report::FS_READ_DENIED.contains("turn service"),
        "{}",
        report::FS_READ_DENIED
    );
}

#[test]
fn ledger_arithmetic_property() {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    for _ in 0..1000 {
        let added = xorshift(&mut state) % 50;
        let deleted = xorshift(&mut state) % 50;
        let files = (xorshift(&mut state) % 5) as usize;
        let new_files = xorshift(&mut state) % (u64::try_from(files).unwrap_or(u64::MAX) + 1);
        let new_files = usize::try_from(new_files).unwrap_or(0);
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
        let mut stopped = false;
        let steps = (xorshift(&mut state) % 8) + 1;
        for sequence in 1..=steps {
            let key = [(xorshift(&mut state) % 4) as u8; 16];
            let ok = xorshift(&mut state).is_multiple_of(2);
            let got = stream.on_call(key, u128::from(sequence));
            if !stopped {
                if last == Some(key) {
                    expected = expected.saturating_add(if errored { 2 } else { 1 }).min(3);
                } else {
                    last = Some(key);
                    expected = 1;
                }
                stopped = expected >= 3;
            }
            let want = if stopped {
                strike::Step::Stop
            } else {
                strike::Step::Run { strikes: expected }
            };
            assert_eq!(got, want, "sequence {sequence}");
            stream.on_result(key, u128::from(sequence), ok);
            errored = !ok;
            assert_eq!(stream.stopped(), stopped);
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
        for _ in 0..=xorshift(&mut state) % 3 {
            body.push_str(
                fragments[usize::try_from(
                    xorshift(&mut state) % u64::try_from(fragments.len()).unwrap_or(0),
                )
                .unwrap_or(0)],
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

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How the scripted host answers a service the guard can be refused.
#[derive(Clone, Copy, Default)]
enum Grant {
    #[default]
    Allowed,
    Denied,
    Declined,
    Failed,
}

impl Grant {
    fn refusal(self) -> Option<ServiceError> {
        match self {
            Self::Allowed => None,
            Self::Denied => Some(ServiceError::Denied(DenyReason::NotGranted)),
            Self::Declined => Some(ServiceError::Declined),
            Self::Failed => Some(ServiceError::failed(None, "scripted failure")),
        }
    }
}

/// The host side of the guard's service boundary. It answers `fs.read` and
/// `turn` as scripted and records every operation the guard performs; any
/// other service call is recorded as stray, because the guard uses no other.
#[derive(Default)]
struct ScriptedHost {
    turn_grant: Mutex<Grant>,
    fs_grant: Mutex<Grant>,
    files: Mutex<HashMap<String, Vec<u8>>>,
    turn_ops: Mutex<Vec<TurnOp>>,
    fs_reads: Mutex<Vec<String>>,
    notices: Mutex<Vec<Notice>>,
    stray: Mutex<Vec<&'static str>>,
    hold_reads: Mutex<bool>,
    /// Serves paths no test staged from the real filesystem.
    disk: Mutex<bool>,
    read_entered: Notify,
    read_release: Notify,
}

impl ScriptedHost {
    fn put_file(&self, path: &str, text: &str) {
        locked(&self.files).insert(path.to_owned(), text.as_bytes().to_vec());
    }

    fn turn_ops(&self) -> Vec<TurnOp> {
        locked(&self.turn_ops).clone()
    }

    fn reads(&self) -> Vec<String> {
        locked(&self.fs_reads).clone()
    }

    fn notices(&self) -> Vec<Notice> {
        locked(&self.notices).clone()
    }

    fn stray<T: Send>(&self, name: &'static str) -> ServiceFuture<'_, T> {
        locked(&self.stray).push(name);
        Box::pin(async {
            Err(ServiceError::failed(
                None,
                "the guard never uses this service",
            ))
        })
    }
}

impl Services for ScriptedHost {
    fn fs_read(&self, _who: &Caller, path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        locked(&self.fs_reads).push(path.to_owned());
        let hold = *locked(&self.hold_reads);
        let refusal = locked(&self.fs_grant).refusal();
        let staged = locked(&self.files).get(path).cloned();
        let from_disk = staged.is_none() && *locked(&self.disk);
        let path = path.to_owned();
        Box::pin(async move {
            if hold {
                self.read_entered.notify_one();
                self.read_release.notified().await;
            }
            let bytes = if from_disk {
                tokio::fs::read(&path).await.ok()
            } else {
                staged
            };
            refusal.map_or(Ok(bytes), Err)
        })
    }

    fn turn(&self, _who: &Caller, op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        locked(&self.turn_ops).push(op);
        let refusal = locked(&self.turn_grant).refusal();
        Box::pin(async move { refusal.map_or(Ok(TurnOpReply::Cancelled), Err) })
    }

    fn notify(&self, _who: &Caller, notice: Notice) {
        locked(&self.notices).push(notice);
    }

    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        self.stray("fs_write")
    }

    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        self.stray("net")
    }

    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        self.stray("run")
    }

    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        self.stray("env")
    }

    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        self.stray("ask")
    }

    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        self.stray("mcp")
    }

    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        self.stray("mcp_declarations")
    }

    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        self.stray("add_session_tools")
    }

    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        self.stray("history_texts")
    }

    fn private_outcomes(&self, _who: &Caller) -> ServiceFuture<'_, Vec<dal_core::PrivateEffect>> {
        self.stray("private_outcomes")
    }

    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        self.stray("agents")
    }

    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        self.stray("jobs")
    }

    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        self.stray("open_asks")
    }

    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<Doc>> {
        self.stray("scheme")
    }

    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        self.stray("sidecar")
    }

    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        self.stray("state")
    }

    fn model_ready(
        &self,
        _who: &Caller,
        _model: dal_core::ModelRoute,
    ) -> ServiceFuture<'_, Option<dal_core::ModelRoute>> {
        self.stray("model_ready")
    }

    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        self.stray("infer")
    }

    fn infer_stream(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        self.stray("infer_stream")
    }

    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        self.stray("call_tool")
    }

    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        self.stray("append_record")
    }

    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        self.stray("records")
    }

    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        self.stray("blob_put")
    }

    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        self.stray("blob_get")
    }
}

fn turn_n(value: u64) -> TurnId {
    TurnId::new(NonZeroU64::new(value).expect("nonzero turn"))
}

/// One tool call as the model dispatches it.
struct Call {
    id: String,
    tool: &'static str,
    class: ToolClass,
    args: String,
}

fn call_of(id: &str, tool: &'static str, class: ToolClass, args: &str) -> Call {
    Call {
        id: id.to_owned(),
        tool,
        class,
        args: args.to_owned(),
    }
}

fn patch(id: &str, args: &str) -> Call {
    call_of(id, "patch", ToolClass::Patch, args)
}

fn search(id: &str, args: &str) -> Call {
    call_of(id, "search", ToolClass::Read, args)
}

fn exec(id: &str, args: &str) -> Call {
    let class = ToolClass::Exec {
        read_only: false,
        grant: None,
    };
    call_of(id, "exec", class, args)
}

fn read_only_exec(id: &str, args: &str) -> Call {
    let class = ToolClass::Exec {
        read_only: true,
        grant: None,
    };
    call_of(id, "exec", class, args)
}

/// The verdict the guard owes every mutating call of an exhausted turn.
fn exhausted(tool: &str, args: &str, cause: &str, paths: &str) -> ToolCallVerdict {
    let name: Name = tool.parse().expect("tool name");
    let key = strike_key(&name, &RawJson::parse(args).expect("args"));
    ToolCallVerdict::Block {
        reason: report::exhaustion(&strike::display_key(tool, &key), cause, paths).into(),
    }
}

fn denial_notice(turn: u64, kind: &str, text: &str) -> Notice {
    Notice {
        turn: Some(turn_n(turn)),
        kind: kind.into(),
        text: text.into(),
    }
}

/// The real guard hooks over one engine, driven the way the host drives them
/// against a scripted host.
struct Rig {
    engine: Arc<Engine>,
    host: Arc<ScriptedHost>,
    session: SessionId,
}

impl Rig {
    async fn open(cfg: GuardConfig) -> Self {
        let rig = Self {
            engine: Arc::new(Engine::new(cfg)),
            host: Arc::new(ScriptedHost::default()),
            session: session_id(),
        };
        let start = SessionStart {
            session: rig.session,
            workspace: Workspace::new(std::env::temp_dir()).expect("absolute workspace"),
            resumed: false,
        };
        SessionStartHook(Arc::clone(&rig.engine))
            .call(start, rig.cx(None))
            .await
            .expect("session_start");
        rig
    }

    fn cx(&self, turn: Option<TurnId>) -> HookCx {
        let services: Arc<dyn Services> = self.host.clone();
        HookCx::for_test(services, self.session, turn)
    }

    async fn begin(&self, turn: u64, text: &str) -> Option<String> {
        let turn = turn_n(turn);
        let input = BeforeTurn {
            turn,
            text: text.into(),
        };
        BeforeTurnHook(Arc::clone(&self.engine))
            .call(input, self.cx(Some(turn)))
            .await
            .expect("before_turn")
    }

    async fn call(&self, turn: u64, call: &Call) -> ToolCallVerdict {
        let turn = turn_n(turn);
        let event = ToolCallEvent {
            turn,
            call: CallId::new(call.id.as_str()),
            tool: call.tool.parse().expect("tool name"),
            class: call.class.clone(),
            args: RawJson::parse(&call.args).expect("args"),
        };
        ToolCallHook(Arc::clone(&self.engine))
            .call(event, self.cx(Some(turn)))
            .await
            .expect("tool_call")
    }

    fn result_event(
        turn: TurnId,
        id: &str,
        tool: &str,
        ok: bool,
        preview: &str,
    ) -> ToolResultEvent {
        ToolResultEvent {
            turn,
            call: CallId::new(id),
            tool: tool.parse().expect("tool name"),
            ok,
            preview: preview.into(),
        }
    }

    async fn result(&self, turn: u64, id: &str, tool: &str, ok: bool, preview: &str) {
        let turn = turn_n(turn);
        ToolResultHook(Arc::clone(&self.engine))
            .call(
                Self::result_event(turn, id, tool, ok, preview),
                self.cx(Some(turn)),
            )
            .await
            .expect("tool_result");
    }

    async fn end(&self, turn: u64) {
        let turn = turn_n(turn);
        let input = TurnEnd {
            turn,
            stop: Stop::EndTurn,
            overflowed: false,
        };
        TurnEndHook(Arc::clone(&self.engine))
            .call(input, self.cx(Some(turn)))
            .await
            .expect("turn_end");
    }

    fn findings(&self) -> Arc<GuardFindings> {
        FindingsHandle(Arc::clone(&self.engine))
            .last(&self.session)
            .expect("findings of an ended turn")
    }

    /// Records `path` as changed in `turn` through the real patch observer.
    fn touch(&self, turn: u64, path: &str) {
        let hunks = [DiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            lines: vec![DiffLine {
                kind: DiffLineKind::Added,
                text: "x".into(),
            }],
        }];
        let file = StagedFile {
            path: Path::new(path),
            absolute_path: Path::new(path),
            before: Some(b"old".as_ref()),
            after: Some(b"new"),
            hunks: &hunks,
            pre_parse: None,
            post_parse: None,
        };
        let batch = StagedBatch {
            session: self.session,
            turn: turn_n(turn),
            call: CallId::new(format!("touch-{path}")),
            files: vec![file],
        };
        let out = self.engine.inspect(&batch);
        assert!(out.is_empty(), "{out:?}");
    }

    fn assert_no_stray_service_use(&self) {
        let stray = locked(&self.host.stray).clone();
        assert!(stray.is_empty(), "the guard used {stray:?}");
    }
}

/// Dispatches three identical mutating calls; the first two must run.
async fn exhaust(rig: &Rig, turn: u64, args: &str) -> ToolCallVerdict {
    for id in ["e1", "e2"] {
        let verdict = rig.call(turn, &patch(id, args)).await;
        assert_eq!(verdict, ToolCallVerdict::Allow);
        rig.result(turn, id, "patch", true, "applied").await;
    }
    rig.call(turn, &patch("e3", args)).await
}

const SAME: &str = r#"{"path":"a.rs","new_text":"x"}"#;
const A_WARNING: &str = "warning: unused variable: `x`\n   --> src/a.rs:12:9\n";
const B_WARNING: &str = "warning: unused variable: `y`\n   --> src/b.rs:30:5\n";
const CARGO_BUILD: &str = r#"{"command":"cargo build"}"#;

#[tokio::test]
async fn three_identical_mutating_calls_block_and_cancel_the_turn_once() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "edit a").await;
    rig.touch(1, "a.rs");
    let third = exhaust(&rig, 1, SAME).await;
    assert_eq!(third, exhausted("patch", SAME, "none", "a.rs"));
    assert_eq!(rig.host.turn_ops(), vec![TurnOp::Cancel]);
    // The fourth identical call is blocked too and asks for no second cancel.
    assert_eq!(rig.call(1, &patch("e4", SAME)).await, third);
    assert_eq!(rig.host.turn_ops(), vec![TurnOp::Cancel]);
    assert_eq!(rig.host.notices(), [] as [Notice; 0]);
    rig.assert_no_stray_service_use();
}

#[tokio::test]
async fn an_errored_repeat_blocks_the_second_call_after_a_strike_notice() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "fix it").await;
    let args = r#"{"path":"a.rs"}"#;
    assert_eq!(
        rig.call(1, &patch("c1", args)).await,
        ToolCallVerdict::Allow
    );
    rig.result(1, "c1", "patch", false, "syntax error at line 3")
        .await;
    assert_eq!(
        rig.call(1, &patch("c2", args)).await,
        exhausted("patch", args, "syntax error at line 3", "none")
    );
    assert_eq!(rig.host.turn_ops(), vec![TurnOp::Cancel]);
    rig.end(1).await;
    let next = rig.begin(2, "again").await.expect("strike report");
    let effect = "patch returned an error";
    let notice = report::strike_notice(1, effect, "syntax error at line 3", "a.rs");
    assert!(next.contains(&notice), "{next}");
}

#[tokio::test]
async fn exhaustion_persists_through_changed_arguments_and_spares_other_calls() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "edit a").await;
    let stopped = exhaust(&rig, 1, SAME).await;
    assert_eq!(stopped, exhausted("patch", SAME, "none", "none"));
    let eval = call_of("p3", "eval", ToolClass::Eval { pure: false }, "{}");
    let other_tool = [
        patch("p1", r#"{"path":"b.rs"}"#),
        exec("p2", CARGO_BUILD),
        eval,
    ];
    for call in &other_tool {
        assert_eq!(rig.call(1, call).await, stopped, "{}", call.id);
    }
    let pure_eval = call_of("r4", "eval", ToolClass::Eval { pure: true }, "{}");
    let unclassified = call_of("r3", "other", ToolClass::Other, "{}");
    let spared = [
        search("r1", "{}"),
        read_only_exec("r2", r#"{"command":"ls"}"#),
        unclassified,
        pure_eval,
    ];
    for call in &spared {
        assert_eq!(
            rig.call(1, call).await,
            ToolCallVerdict::Allow,
            "{}",
            call.id
        );
    }
    assert_eq!(rig.host.turn_ops(), vec![TurnOp::Cancel]);
}

#[tokio::test]
async fn read_only_calls_never_count_toward_exhaustion() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "look around").await;
    for index in 0..10 {
        let id = format!("r{index}");
        assert_eq!(
            rig.call(1, &search(&id, r#"{"query":"x"}"#)).await,
            ToolCallVerdict::Allow
        );
        rig.result(1, &id, "search", true, "found").await;
        let id = format!("l{index}");
        let listing = read_only_exec(&id, r#"{"command":"ls"}"#);
        assert_eq!(rig.call(1, &listing).await, ToolCallVerdict::Allow);
    }
    rig.end(1).await;
    assert_eq!(rig.findings().strikes, 0);
    assert_eq!(rig.host.turn_ops(), [] as [TurnOp; 0]);
}

#[tokio::test]
async fn a_changed_call_resets_the_count_while_canonical_arguments_still_repeat() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "tune").await;
    for round in 0..3 {
        for (suffix, args) in [("a", r#"{"n": 1.234}"#), ("b", r#"{"n": 1.244}"#)] {
            let id = format!("c{round}{suffix}");
            assert_eq!(
                rig.call(1, &patch(&id, args)).await,
                ToolCallVerdict::Allow,
                "{id}"
            );
            rig.result(1, &id, "patch", true, "applied").await;
        }
    }
    assert_eq!(rig.host.turn_ops(), [] as [TurnOp; 0]);

    // Key order, outer whitespace, and the third significant digit of a
    // number do not make a call different.
    rig.end(1).await;
    rig.begin(2, "tune again").await;
    let first = r#"{"path":"a.rs","n":1.234}"#;
    let spellings = [first, r#"{"n":1.2344,"path":" a.rs "}"#];
    for (index, args) in spellings.into_iter().enumerate() {
        let id = format!("s{index}");
        let verdict = rig.call(2, &patch(&id, args)).await;
        assert_eq!(verdict, ToolCallVerdict::Allow);
        rig.result(2, &id, "patch", true, "applied").await;
    }
    let third = rig
        .call(2, &patch("s2", r#"{"path":"a.rs","n":1.2341}"#))
        .await;
    assert_eq!(third, exhausted("patch", first, "none", "none"));
}

#[tokio::test]
async fn a_new_turn_starts_with_a_fresh_count() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "edit a").await;
    assert_eq!(
        exhaust(&rig, 1, SAME).await,
        exhausted("patch", SAME, "none", "none")
    );
    rig.end(1).await;
    rig.begin(2, "edit a again").await;
    // `exhaust` itself requires the first two calls of the turn to run.
    assert_eq!(
        exhaust(&rig, 2, SAME).await,
        exhausted("patch", SAME, "none", "none")
    );
    assert_eq!(rig.host.turn_ops(), vec![TurnOp::Cancel, TurnOp::Cancel]);
}

#[tokio::test]
async fn a_withheld_turn_service_still_blocks_and_is_reported_once_per_session() {
    for grant in [Grant::Denied, Grant::Declined] {
        let rig = Rig::open(test_config()).await;
        *locked(&rig.host.turn_grant) = grant;
        rig.begin(1, "edit a").await;
        let stopped = exhaust(&rig, 1, SAME).await;
        assert_eq!(stopped, exhausted("patch", SAME, "none", "none"));
        let reported = vec![denial_notice(1, "guard_turn_denied", report::TURN_DENIED)];
        assert_eq!(rig.host.notices(), reported);
        // The turn goes on: reads pass, further mutating calls stay blocked.
        assert_eq!(
            rig.call(1, &search("r1", "{}")).await,
            ToolCallVerdict::Allow
        );
        let changed = patch("p1", r#"{"path":"b.rs"}"#);
        assert_eq!(rig.call(1, &changed).await, stopped);
        assert_eq!(rig.host.turn_ops(), vec![TurnOp::Cancel]);
        // A later turn is refused again, but the session hears of it once.
        rig.end(1).await;
        rig.begin(2, "edit a again").await;
        assert_eq!(exhaust(&rig, 2, SAME).await, stopped);
        assert_eq!(rig.host.turn_ops(), vec![TurnOp::Cancel, TurnOp::Cancel]);
        assert_eq!(rig.host.notices(), reported);
    }
}

#[tokio::test]
async fn a_cancel_that_fails_without_a_denial_is_not_reported_as_one() {
    let rig = Rig::open(test_config()).await;
    *locked(&rig.host.turn_grant) = Grant::Failed;
    rig.begin(1, "edit a").await;
    assert_eq!(
        exhaust(&rig, 1, SAME).await,
        exhausted("patch", SAME, "none", "none")
    );
    assert_eq!(rig.host.turn_ops(), vec![TurnOp::Cancel]);
    assert_eq!(rig.host.notices(), [] as [Notice; 0]);
}

#[tokio::test]
async fn granted_spill_output_is_scanned_for_warnings() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "build").await;
    rig.touch(1, "src/a.rs");
    let log = minted_log("/s");
    rig.host.put_file(&log, A_WARNING);
    assert_eq!(
        rig.call(1, &exec("c1", CARGO_BUILD)).await,
        ToolCallVerdict::Allow
    );
    let preview = format!("Compiling\nFull output: {log}");
    rig.result(1, "c1", "exec", true, &preview).await;
    assert_eq!(rig.host.reads(), vec![log]);
    rig.end(1).await;
    let findings = rig.findings();
    assert_eq!(findings.warnings.len(), 1, "{:?}", findings.warnings);
    assert_eq!(
        (findings.warnings[0].0.as_ref(), findings.warnings[0].1),
        ("src/a.rs", 12)
    );
    let text = findings.report.as_deref().expect("turn report");
    assert!(text.contains("NEW WARNING NOTICE"), "{text}");
    assert_eq!(rig.host.notices(), [] as [Notice; 0]);
    rig.assert_no_stray_service_use();
}

#[tokio::test]
async fn a_withheld_spill_read_scans_previews_and_is_reported_once_per_session() {
    for grant in [Grant::Denied, Grant::Declined] {
        let rig = Rig::open(test_config()).await;
        *locked(&rig.host.fs_grant) = grant;
        rig.begin(1, "build").await;
        rig.touch(1, "src/a.rs");
        rig.touch(1, "src/b.rs");
        let first_log = minted_log("/s");
        assert_eq!(
            rig.call(1, &exec("c1", CARGO_BUILD)).await,
            ToolCallVerdict::Allow
        );
        let first = format!("{A_WARNING}Full output: {first_log}");
        rig.result(1, "c1", "exec", true, &first).await;
        let reported = vec![denial_notice(
            1,
            "guard_fs_read_denied",
            report::FS_READ_DENIED,
        )];
        assert_eq!(rig.host.notices(), reported);
        assert_eq!(
            rig.call(1, &exec("c2", CARGO_BUILD)).await,
            ToolCallVerdict::Allow
        );
        let second_log = minted_log("/s");
        let second = format!("{B_WARNING}Full output: {second_log}");
        rig.result(1, "c2", "exec", true, &second).await;
        // The router stopped asking and stopped telling, yet kept scanning.
        assert_eq!(rig.host.reads(), vec![first_log]);
        assert_eq!(rig.host.notices(), reported);
        rig.end(1).await;
        let found: Vec<_> = rig
            .findings()
            .warnings
            .iter()
            .map(|warning| (warning.0.to_string(), warning.1))
            .collect();
        assert_eq!(
            found,
            vec![("src/a.rs".to_owned(), 12), ("src/b.rs".to_owned(), 30)]
        );
    }
}

#[tokio::test]
async fn a_spill_read_that_fails_without_a_denial_keeps_the_router_reading() {
    let rig = Rig::open(test_config()).await;
    *locked(&rig.host.fs_grant) = Grant::Failed;
    rig.begin(1, "build").await;
    rig.touch(1, "src/a.rs");
    let failed_logs = [minted_log("/s"), minted_log("/s")];
    for (id, log) in [("c1", &failed_logs[0]), ("c2", &failed_logs[1])] {
        assert_eq!(
            rig.call(1, &exec(id, CARGO_BUILD)).await,
            ToolCallVerdict::Allow
        );
        let preview = format!("{A_WARNING}Full output: {log}");
        rig.result(1, id, "exec", true, &preview).await;
    }
    assert_eq!(rig.host.reads().len(), 2);
    assert_eq!(rig.host.notices(), [] as [Notice; 0]);
}

#[tokio::test]
async fn previews_without_a_spill_line_are_scanned_without_a_read() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "build").await;
    rig.touch(1, "src/a.rs");
    assert_eq!(
        rig.call(1, &exec("c1", CARGO_BUILD)).await,
        ToolCallVerdict::Allow
    );
    rig.result(1, "c1", "exec", true, A_WARNING).await;
    assert_eq!(rig.host.reads(), [] as [String; 0]);
    rig.end(1).await;
    assert_eq!(rig.findings().warnings.len(), 1);
}

#[tokio::test]
async fn grep_like_commands_are_never_scanned() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "search the log").await;
    rig.touch(1, "src/a.rs");
    let log = minted_log("/s");
    rig.host.put_file(&log, A_WARNING);
    let grep = exec("c1", r#"{"command":"grep -rn unused ."}"#);
    assert_eq!(rig.call(1, &grep).await, ToolCallVerdict::Allow);
    let preview = format!("{A_WARNING}Full output: {log}");
    rig.result(1, "c1", "exec", true, &preview).await;
    assert_eq!(rig.host.reads(), [] as [String; 0]);
    rig.end(1).await;
    assert_eq!(
        rig.findings().warnings,
        [] as [(Box<str>, u32, Box<str>); 0]
    );
}

#[tokio::test]
async fn only_host_shaped_spill_logs_are_ever_read() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "build").await;
    rig.touch(1, "src/a.rs");
    let forged = [
        ("c1", "/etc/secret"),
        ("c2", "/s/jobs/other.log"),
        ("c3", "/s/cache/c3.log"),
        ("c4", "/s/jobs/c4.log"),
    ];
    for (id, path) in forged {
        rig.host.put_file(path, A_WARNING);
        let build = read_only_exec(id, CARGO_BUILD);
        assert_eq!(rig.call(1, &build).await, ToolCallVerdict::Allow);
        rig.result(1, id, "exec", true, &format!("Full output: {path}"))
            .await;
    }
    // A line forged in command output never makes the guard read for it, and
    // the previews held no warning, so nothing was found either.
    assert_eq!(rig.host.reads(), [] as [String; 0]);
    rig.end(1).await;
    assert_eq!(
        rig.findings().warnings,
        [] as [(Box<str>, u32, Box<str>); 0]
    );
}

/// A spill log path as the real exec tool names it: the host-minted job,
/// never the provider call id.
fn minted_log(dir: &str) -> String {
    format!("{dir}/jobs/{}.log", dal_core::JobId::new_v7())
}

#[tokio::test]
async fn a_minted_job_log_is_read() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "build").await;
    rig.touch(1, "src/a.rs");
    let log = minted_log("/data/sessions/w/s");
    rig.host.put_file(&log, A_WARNING);
    let build = exec("exec-call", CARGO_BUILD);
    assert_eq!(rig.call(1, &build).await, ToolCallVerdict::Allow);
    let preview = format!("Compiling\nFull output: {log}");
    rig.result(1, "exec-call", "exec", true, &preview).await;
    assert_eq!(rig.host.reads(), vec![log]);
    rig.end(1).await;
    let findings = rig.findings();
    assert_eq!(findings.warnings.len(), 1, "{:?}", findings.warnings);
    assert_eq!(rig.host.notices(), [] as [Notice; 0]);
    rig.assert_no_stray_service_use();
}

#[tokio::test]
async fn the_last_footer_line_names_the_log_not_a_line_the_command_printed() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "build").await;
    rig.touch(1, "src/a.rs");
    rig.touch(1, "src/b.rs");
    let forged = minted_log("/data/sessions/w/s");
    let real = minted_log("/data/sessions/w/s");
    rig.host.put_file(&forged, A_WARNING);
    rig.host.put_file(&real, B_WARNING);
    let build = exec("exec-call", CARGO_BUILD);
    assert_eq!(rig.call(1, &build).await, ToolCallVerdict::Allow);
    // The command printed a footer-shaped line of its own before the tool
    // appended the real footer and the status line.
    let preview = format!(
        "Full output: {forged}\nCompiling\nFull output: {real}\nCommand exited with code 1"
    );
    rig.result(1, "exec-call", "exec", false, &preview).await;
    assert_eq!(rig.host.reads(), vec![real]);
    rig.end(1).await;
    let found: Vec<_> = rig
        .findings()
        .warnings
        .iter()
        .map(|warning| (warning.0.to_string(), warning.1))
        .collect();
    assert_eq!(found, vec![("src/b.rs".to_owned(), 30)]);
}

#[tokio::test]
async fn a_spill_file_on_disk_surfaces_the_warning_the_preview_tail_lost() {
    let dir = tempfile::tempdir().expect("scratch directory");
    let jobs = dir.path().join("jobs");
    std::fs::create_dir(&jobs).expect("jobs directory");
    let log = jobs.join(format!("{}.log", dal_core::JobId::new_v7()));
    // What a long build leaves behind: a warning at the head, then output
    // far beyond the 4 KiB tail the tool returns inline.
    let mut filler = String::new();
    for line in 0..300 {
        let _ = writeln!(
            filler,
            "   Compiling crate-{line} v0.1.0 (/work/crate-{line})"
        );
    }
    let full = format!("{A_WARNING}{filler}");
    std::fs::write(&log, &full).expect("write the spill file");
    let tail = &full[full.len() - 4096..];
    let rig = Rig::open(test_config()).await;
    *locked(&rig.host.disk) = true;
    rig.begin(1, "build").await;
    rig.touch(1, "src/a.rs");
    assert_eq!(
        rig.call(1, &exec("exec-call", CARGO_BUILD)).await,
        ToolCallVerdict::Allow
    );
    assert!(
        !tail.contains("unused variable"),
        "the tail lost the warning"
    );
    let preview = format!("{tail}\nFull output: {}", log.display());
    rig.result(1, "exec-call", "exec", true, &preview).await;
    assert_eq!(rig.host.reads(), vec![log.display().to_string()]);
    rig.end(1).await;
    let findings = rig.findings();
    assert_eq!(
        findings
            .warnings
            .iter()
            .map(|warning| (warning.0.to_string(), warning.1))
            .collect::<Vec<_>>(),
        vec![("src/a.rs".to_owned(), 12)]
    );
    assert_eq!(rig.host.notices(), [] as [Notice; 0]);
    rig.assert_no_stray_service_use();
}

#[tokio::test]
async fn spill_log_text_replaces_preview_text() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "build").await;
    rig.touch(1, "src/a.rs");
    rig.touch(1, "src/b.rs");
    let log = minted_log("/s");
    rig.host.put_file(&log, B_WARNING);
    assert_eq!(
        rig.call(1, &exec("c1", CARGO_BUILD)).await,
        ToolCallVerdict::Allow
    );
    // Only the served log is scanned.
    let preview = format!("{A_WARNING}Full output: {log}");
    rig.result(1, "c1", "exec", true, &preview).await;
    assert_eq!(rig.host.reads(), vec![log]);
    rig.end(1).await;
    let found: Vec<_> = rig
        .findings()
        .warnings
        .iter()
        .map(|warning| (warning.0.to_string(), warning.1))
        .collect();
    assert_eq!(found, vec![("src/b.rs".to_owned(), 30)]);
    rig.assert_no_stray_service_use();
}

#[tokio::test]
async fn missing_spill_file_falls_back_to_preview() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "build").await;
    rig.touch(1, "src/a.rs");
    assert_eq!(
        rig.call(1, &exec("c1", CARGO_BUILD)).await,
        ToolCallVerdict::Allow
    );
    // No log staged: the preview is still scanned, nothing withheld.
    let log = minted_log("/s");
    let preview = format!("{A_WARNING}Full output: {log}");
    rig.result(1, "c1", "exec", true, &preview).await;
    assert_eq!(rig.host.reads(), vec![log]);
    assert_eq!(rig.host.notices(), [] as [Notice; 0]);
    rig.end(1).await;
    let found: Vec<_> = rig
        .findings()
        .warnings
        .iter()
        .map(|warning| (warning.0.to_string(), warning.1))
        .collect();
    assert_eq!(found, vec![("src/a.rs".to_owned(), 12)]);
    rig.assert_no_stray_service_use();
}

#[tokio::test]
async fn failed_spill_read_keeps_preview_warnings_and_keeps_reading() {
    let rig = Rig::open(test_config()).await;
    *locked(&rig.host.fs_grant) = Grant::Failed;
    rig.begin(1, "build").await;
    rig.touch(1, "src/a.rs");
    rig.touch(1, "src/b.rs");
    let kept_logs = [minted_log("/s"), minted_log("/s")];
    for (id, warning, log) in [
        ("c1", A_WARNING, &kept_logs[0]),
        ("c2", B_WARNING, &kept_logs[1]),
    ] {
        assert_eq!(
            rig.call(1, &exec(id, CARGO_BUILD)).await,
            ToolCallVerdict::Allow
        );
        let preview = format!("{warning}Full output: {log}");
        rig.result(1, id, "exec", true, &preview).await;
    }
    // A failure that withholds nothing is silent, yet every later spill is
    // still attempted and every preview still scanned.
    assert_eq!(rig.host.reads(), kept_logs.to_vec());
    assert_eq!(rig.host.notices(), [] as [Notice; 0]);
    rig.end(1).await;
    let found: Vec<_> = rig
        .findings()
        .warnings
        .iter()
        .map(|warning| (warning.0.to_string(), warning.1))
        .collect();
    assert_eq!(
        found,
        vec![("src/a.rs".to_owned(), 12), ("src/b.rs".to_owned(), 30)]
    );
    rig.assert_no_stray_service_use();
}

#[tokio::test]
async fn exhaustion_reason_is_frozen_at_the_first_stop() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "edit").await;
    // A different edit goes out first and stays in flight past the stop.
    assert_eq!(
        rig.call(1, &patch("b1", r#"{"path":"b.rs"}"#)).await,
        ToolCallVerdict::Allow
    );
    let stopped = exhaust(&rig, 1, SAME).await;
    assert_eq!(stopped, exhausted("patch", SAME, "none", "none"));
    // After the stop the in-flight edit fails and its file lands, but later
    // blocked calls still carry the reason rendered at the stop.
    rig.result(1, "b1", "patch", false, "late failure").await;
    rig.touch(1, "b.rs");
    assert_eq!(rig.call(1, &patch("e4", SAME)).await, stopped);
    assert_eq!(rig.host.turn_ops(), vec![TurnOp::Cancel]);
    assert_eq!(rig.host.notices(), [] as [Notice; 0]);
    rig.assert_no_stray_service_use();
}

type ParkedHook = tokio::task::JoinSet<Result<(), HookError>>;

/// Parks the result hook of exec call `c1` inside the host's `fs.read`, then
/// ends its turn and starts another that reuses the turn id, as a resumed
/// session does. The read is still pending when this returns.
async fn park_read_across_turns(rig: &Rig) -> ParkedHook {
    rig.begin(1, "build").await;
    let log = minted_log("/s");
    rig.host.put_file(&log, A_WARNING);
    *locked(&rig.host.hold_reads) = true;
    let build = exec("c1", CARGO_BUILD);
    assert_eq!(rig.call(1, &build).await, ToolCallVerdict::Allow);
    let hook = ToolResultHook(Arc::clone(&rig.engine));
    let preview = format!("Full output: {log}");
    let event = Rig::result_event(turn_n(1), "c1", "exec", true, &preview);
    let cx = rig.cx(Some(turn_n(1)));
    let mut pending = tokio::task::JoinSet::new();
    pending.spawn(async move { hook.call(event, cx).await });
    tokio::time::timeout(Duration::from_secs(5), rig.host.read_entered.notified())
        .await
        .expect("the result hook reached fs.read");
    // Parked inside the host read, the hook holds no engine lock.
    assert!(rig.engine.state.try_lock().is_ok());
    rig.end(1).await;
    rig.begin(1, "next").await;
    pending
}

/// Lets the parked read complete and waits for its hook to finish.
async fn finish_parked_read(rig: &Rig, mut pending: ParkedHook) {
    rig.host.read_release.notify_one();
    pending
        .join_next()
        .await
        .expect("result hook task")
        .expect("join hook task")
        .expect("tool_result");
}

#[tokio::test]
async fn a_spill_read_that_outlives_its_turn_leaves_the_next_turn_alone() {
    let rig = Rig::open(test_config()).await;
    let pending = park_read_across_turns(&rig).await;
    finish_parked_read(&rig, pending).await;
    assert_eq!(rig.host.reads().len(), 1);
    rig.end(1).await;
    assert_eq!(
        rig.findings().warnings,
        [] as [(Box<str>, u32, Box<str>); 0]
    );
}

#[tokio::test]
async fn a_withheld_spill_read_that_outlives_its_turn_still_ends_spill_reads() {
    let rig = Rig::open(test_config()).await;
    *locked(&rig.host.fs_grant) = Grant::Declined;
    let pending = park_read_across_turns(&rig).await;
    finish_parked_read(&rig, pending).await;
    // The refusal belongs to the session: reported once, no turn-1 warning
    // lands in the new turn, and the next exec asks the host nothing.
    let reported = vec![denial_notice(
        1,
        "guard_fs_read_denied",
        report::FS_READ_DENIED,
    )];
    assert_eq!(rig.host.notices(), reported);
    let next = exec("c2", CARGO_BUILD);
    assert_eq!(rig.call(1, &next).await, ToolCallVerdict::Allow);
    let next_preview = format!("Full output: {}", minted_log("/s"));
    rig.result(1, "c2", "exec", true, &next_preview).await;
    assert_eq!(rig.host.reads().len(), 1);
    assert_eq!(rig.host.notices(), reported);
    rig.end(1).await;
    assert_eq!(
        rig.findings().warnings,
        [] as [(Box<str>, u32, Box<str>); 0]
    );
}

#[tokio::test]
async fn a_disabled_guard_never_blocks_or_calls_a_service() {
    let rig = Rig::open(GuardConfig::disabled()).await;
    assert_eq!(rig.begin(1, "edit a").await, None);
    for id in ["e1", "e2", "e3", "e4"] {
        let verdict = rig.call(1, &patch(id, SAME)).await;
        assert_eq!(verdict, ToolCallVerdict::Allow);
    }
    let log = minted_log("/s");
    rig.host.put_file(&log, A_WARNING);
    let preview = format!("Full output: {log}");
    rig.result(1, "e1", "exec", true, &preview).await;
    assert_eq!(rig.host.turn_ops(), [] as [TurnOp; 0]);
    assert_eq!(rig.host.reads(), [] as [String; 0]);
    assert_eq!(rig.host.notices(), [] as [Notice; 0]);
    rig.assert_no_stray_service_use();
}

/// Opens a second real session on `rig`'s engine through the genuine session
/// hook: same engine, same host contracts, different session identity.
async fn open_peer(rig: &Rig) -> SessionId {
    let session = session_id();
    let start = SessionStart {
        session,
        workspace: Workspace::new(std::env::temp_dir()).expect("absolute workspace"),
        resumed: false,
    };
    let services: Arc<dyn Services> = rig.host.clone();
    SessionStartHook(Arc::clone(&rig.engine))
        .call(start, HookCx::for_test(services, session, None))
        .await
        .expect("peer session_start");
    session
}

fn peer_cx(rig: &Rig, session: SessionId, turn: TurnId) -> HookCx {
    let services: Arc<dyn Services> = rig.host.clone();
    HookCx::for_test(services, session, Some(turn))
}

async fn peer_begin(rig: &Rig, session: SessionId, turn: u64, text: &str) -> Option<String> {
    let turn_id = turn_n(turn);
    let input = BeforeTurn {
        turn: turn_id,
        text: text.into(),
    };
    BeforeTurnHook(Arc::clone(&rig.engine))
        .call(input, peer_cx(rig, session, turn_id))
        .await
        .expect("peer before_turn")
}

async fn peer_call(rig: &Rig, session: SessionId, turn: u64, call: &Call) -> ToolCallVerdict {
    let turn_id = turn_n(turn);
    let event = ToolCallEvent {
        turn: turn_id,
        call: CallId::new(call.id.as_str()),
        tool: call.tool.parse().expect("tool name"),
        class: call.class.clone(),
        args: RawJson::parse(&call.args).expect("args"),
    };
    ToolCallHook(Arc::clone(&rig.engine))
        .call(event, peer_cx(rig, session, turn_id))
        .await
        .expect("peer tool_call")
}

async fn peer_end(rig: &Rig, session: SessionId, turn: u64) {
    let turn_id = turn_n(turn);
    let input = TurnEnd {
        turn: turn_id,
        stop: Stop::EndTurn,
        overflowed: false,
    };
    TurnEndHook(Arc::clone(&rig.engine))
        .call(input, peer_cx(rig, session, turn_id))
        .await
        .expect("peer turn_end");
}

async fn close_session(rig: &Rig, session: SessionId) {
    let input = SessionEnd {
        session,
        reason: "done".into(),
    };
    let services: Arc<dyn Services> = rig.host.clone();
    SessionEndHook(Arc::clone(&rig.engine))
        .call(input, HookCx::for_test(services, session, None))
        .await
        .expect("session_end");
}

fn peer_findings(rig: &Rig, session: &SessionId) -> Arc<GuardFindings> {
    FindingsHandle(Arc::clone(&rig.engine))
        .last(session)
        .expect("findings of an ended peer turn")
}

fn todo_feed() -> (&'static str, Channel) {
    let name: Name = "patch".parse().expect("tool name");
    (
        "{\"new_text\":\"// TODO\"}\n",
        Channel::ToolArgs { tool: name },
    )
}

#[tokio::test]
async fn same_turn_id_in_two_sessions_keeps_watchers_and_findings_separate() {
    let rig = Rig::open(test_config()).await;
    let peer = open_peer(&rig).await;
    rig.begin(1, "edit a").await;
    peer_begin(&rig, peer, 1, "edit b").await;
    let router = super::stream::Router::new(Arc::clone(&rig.engine));
    let mut watch_mine = router
        .start(&TurnInfo::new(rig.session, turn_n(1)))
        .expect("own session starts a watcher");
    let mut watch_peer = router
        .start(&TurnInfo::new(peer, turn_n(1)))
        .expect("peer session starts its own watcher");
    let (line, channel) = todo_feed();
    let verdict = watch_mine.feed(channel, line);
    assert!(matches!(verdict, dal_core::StreamVerdict::Continue));
    watch_mine.finish();
    watch_peer.finish();
    rig.end(1).await;
    peer_end(&rig, peer, 1).await;
    assert_eq!(rig.findings().stream, vec![(G8Rule::BareTodo, 1)]);
    assert_eq!(peer_findings(&rig, &peer).stream, [] as [(G8Rule, u32); 0]);
}

#[tokio::test]
async fn repeat_guard_counts_stay_per_session_when_turn_ids_collide() {
    let rig = Rig::open(test_config()).await;
    let peer = open_peer(&rig).await;
    rig.begin(1, "edit a").await;
    peer_begin(&rig, peer, 1, "edit b").await;
    let third = exhaust(&rig, 1, SAME).await;
    assert!(matches!(third, ToolCallVerdict::Block { .. }));
    let verdict = peer_call(&rig, peer, 1, &patch("p1", SAME)).await;
    assert_eq!(verdict, ToolCallVerdict::Allow);
}

#[tokio::test]
async fn a_stale_watcher_cannot_credit_a_replacement_turn() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "first").await;
    let router = super::stream::Router::new(Arc::clone(&rig.engine));
    let mut stale = router
        .start(&TurnInfo::new(rig.session, turn_n(1)))
        .expect("watch starts");
    let (line, channel) = todo_feed();
    let verdict = stale.feed(channel, line);
    assert!(matches!(verdict, dal_core::StreamVerdict::Continue));
    rig.begin(1, "next").await;
    stale.finish();
    let mut replacement = router
        .start(&TurnInfo::new(rig.session, turn_n(1)))
        .expect("replacement turn starts a watcher");
    let (again, channel) = todo_feed();
    let verdict = replacement.feed(channel, again);
    assert!(matches!(verdict, dal_core::StreamVerdict::Continue));
    replacement.finish();
    rig.end(1).await;
    assert_eq!(rig.findings().stream, vec![(G8Rule::BareTodo, 1)]);
}

#[tokio::test]
async fn a_watcher_cannot_credit_a_closed_session() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "edit").await;
    let router = super::stream::Router::new(Arc::clone(&rig.engine));
    let mut watch = router
        .start(&TurnInfo::new(rig.session, turn_n(1)))
        .expect("watch starts");
    let (line, channel) = todo_feed();
    let verdict = watch.feed(channel, line);
    assert!(matches!(verdict, dal_core::StreamVerdict::Continue));
    close_session(&rig, rig.session).await;
    watch.finish();
    assert!(
        router
            .start(&TurnInfo::new(rig.session, turn_n(1)))
            .is_none()
    );
    assert!(
        FindingsHandle(Arc::clone(&rig.engine))
            .last(&rig.session)
            .is_none()
    );
    let peer = open_peer(&rig).await;
    peer_begin(&rig, peer, 1, "edit").await;
    assert!(router.start(&TurnInfo::new(peer, turn_n(1))).is_some());
}

#[tokio::test]
async fn g1_new_file_detection() {
    let (engine, session, turn) = engine_with_turn(test_config());
    let src = "fn main() {\n}\n";
    let post = parsed("/ws/a.rs", src).await;
    let hunks = [DiffHunk {
        old_start: 1,
        old_lines: 0,
        new_start: 1,
        new_lines: 2,
        lines: vec![
            DiffLine {
                kind: DiffLineKind::Added,
                text: "fn main() {".into(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                text: "}".into(),
            },
        ],
    }];
    let files = [StagedFile {
        path: Path::new("a.rs"),
        absolute_path: Path::new("/ws/a.rs"),
        before: None,
        after: Some(src.as_bytes()),
        hunks: &hunks,
        pre_parse: None,
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
            .all(|finding| finding.severity != FindingSeverity::Block),
        "{out:?}"
    );
    let state = engine.state.lock().expect("engine state");
    let turn = state
        .sessions
        .get(&session)
        .expect("session")
        .turn
        .as_ref()
        .expect("turn");
    assert_eq!((turn.added, turn.deleted), (2, 0));
    assert_eq!(turn.files.len(), 1);
    assert_eq!(turn.new_files.len(), 1);
    assert!(turn.new_files.contains("a.rs"));
}

#[test]
fn config_unknown_g8_rule() {
    let mut section = dal_core::GuardSection::default();
    section.policies.g8_calibrated_rules = vec!["vibes".to_string()];
    let error = GuardConfig::from_section(&section, &Calibration::none())
        .expect_err("unknown g8 rule is rejected");
    assert_eq!(error, GuardConfigError::UnknownRule("vibes".to_string()));
    assert_eq!(error.to_string(), "unknown guard g8 rule vibes");
}

#[test]
fn node_kinds_exist_in_grammar() {
    for language in [
        parse::Language::Rust,
        parse::Language::Ocaml,
        parse::Language::OcamlInterface,
        parse::Language::Python,
        parse::Language::JavaScript,
        parse::Language::TypeScript,
        parse::Language::Tsx,
        parse::Language::Go,
        parse::Language::C,
        parse::Language::Cpp,
    ] {
        let table = super::tables::table(language);
        let grammar = language.grammar();
        let mut kinds = vec![table.bool_node];
        for list in [
            table.functions,
            table.decisions,
            table.nesting,
            table.folded,
            table.flat,
            table.else_parents,
            table.calls,
            table.comments,
            table.identifiers,
            table.labeled_jumps,
        ] {
            kinds.extend(list.iter().copied());
        }
        for kind in kinds {
            assert_ne!(
                grammar.id_for_node_kind(kind, true),
                0,
                "{language:?} has no node kind {kind}"
            );
        }
    }
}

#[tokio::test]
async fn cognitive_recursion_else_if() {
    let plain = "fn f(x: i32) -> i32 {\n if x <= 0 {\n 0\n } else {\n x\n }\n}\n";
    let chained =
        "fn f(x: i32) -> i32 {\n if x <= 0 {\n 0\n } else if x == 1 {\n 1\n } else {\n x\n }\n}\n";
    let recursive = "fn f(x: i32) -> i32 {\n if x <= 0 {\n 0\n } else {\n f(x - 1)\n }\n}\n";
    let plain_tree = parsed("/ws/a.rs", plain).await;
    let plain_cog = metrics::measure(parse::Language::Rust, &plain_tree.tree, plain.as_bytes())
        .functions[0]
        .cognitive;
    let chained_tree = parsed("/ws/a.rs", chained).await;
    let chained_cog = metrics::measure(
        parse::Language::Rust,
        &chained_tree.tree,
        chained.as_bytes(),
    )
    .functions[0]
        .cognitive;
    assert_eq!(
        chained_cog,
        plain_cog + 1,
        "else-if adds exactly one (plain {plain_cog}, chained {chained_cog})"
    );
    let recursive_tree = parsed("/ws/a.rs", recursive).await;
    let recursive_cog = metrics::measure(
        parse::Language::Rust,
        &recursive_tree.tree,
        recursive.as_bytes(),
    )
    .functions[0]
        .cognitive;
    assert_eq!(
        recursive_cog,
        plain_cog + 1,
        "recursion adds exactly one (plain {plain_cog}, recursive {recursive_cog})"
    );
}

#[tokio::test]
async fn cyclomatic_definition() {
    let src = "int f(int x, int y, int z) {\n int r = 0;\n if (x > 0 && y > 0 || z > 0) {\n r = 1;\n }\n switch (x) {\n case 1:\n r += 1;\n break;\n case 2:\n r += 2;\n break;\n case 3:\n r += 3;\n break;\n }\n r += (y > 0) ? 10 : 20;\n return r;\n}\n";
    let post = parsed("/ws/a.c", src).await;
    let measured = metrics::measure(parse::Language::C, &post.tree, src.as_bytes());
    assert_eq!(measured.functions.len(), 1);
    assert_eq!(
        measured.functions[0].cyclomatic, 8,
        "{:?}",
        measured.functions[0]
    );
}

#[tokio::test]
async fn g8_dedup() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "edit").await;
    let router = super::stream::Router::new(Arc::clone(&rig.engine));
    let mut watch = router
        .start(&TurnInfo::new(rig.session, turn_n(1)))
        .expect("watch starts");
    let (first, channel) = todo_feed();
    assert!(matches!(
        watch.feed(channel, first),
        dal_core::StreamVerdict::Continue
    ));
    let (second, channel) = todo_feed();
    assert!(matches!(
        watch.feed(channel, second),
        dal_core::StreamVerdict::Continue
    ));
    watch.finish();
    rig.end(1).await;
    assert_eq!(rig.findings().stream, vec![(G8Rule::BareTodo, 2)]);
}

#[tokio::test]
async fn large_file_skip() {
    let (engine, session, turn) = engine_with_turn(test_config());
    // The gate pairs byte images with already-parsed trees, so the test pairs
    // large byte images with small representative trees instead of parsing
    // 600 KiB under the 250 ms parse budget.
    let big_src = format!("fn main() {{}}\n{}", "// padding\n".repeat(70_000));
    assert!(
        big_src.len() > 524_288,
        "fixture exceeds the large-file limit"
    );
    let big = parsed("/ws/a.rs", "fn main() {}\n").await;
    let bad = parsed("/ws/a.rs", "fn broken( {\n").await;
    let outcome = checks::parse_gate(
        "big.rs",
        Path::new("/ws/big.rs"),
        Some(big_src.as_bytes()),
        format!("{big_src}fn broken( {{\n").as_bytes(),
        Some(&big),
        Some(&bad),
    );
    assert!(
        matches!(outcome, checks::GateOutcome::Reject(_)),
        "{outcome:?}"
    );
    let hunks = [DiffHunk {
        old_start: 1,
        old_lines: 1,
        new_start: 1,
        new_lines: 2,
        lines: vec![DiffLine {
            kind: DiffLineKind::Added,
            text: "fn main() {}".into(),
        }],
    }];
    let files = [StagedFile {
        path: Path::new("big.rs"),
        absolute_path: Path::new("/ws/big.rs"),
        before: None,
        after: Some(big_src.as_bytes()),
        hunks: &hunks,
        pre_parse: None,
        post_parse: Some(big),
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
            .any(|finding| finding.text.contains("skipped (large file)")),
        "{out:?}"
    );
}

#[tokio::test]
async fn non_utf8_skip() {
    let valid = parsed("/ws/a.rs", "fn main() {\n}\n").await;
    let outcome = checks::parse_gate(
        "a.rs",
        Path::new("/ws/a.rs"),
        Some(b"fn main() {\n}\n"),
        b"\xff\xfe",
        Some(&valid),
        Some(&valid),
    );
    assert!(
        matches!(outcome, checks::GateOutcome::Skipped),
        "{outcome:?}"
    );
    let control = checks::parse_gate(
        "a.rs",
        Path::new("/ws/a.rs"),
        Some(b"fn main() {\n}\n"),
        b"fn main() {\n}\n",
        Some(&valid),
        Some(&valid),
    );
    assert!(
        matches!(control, checks::GateOutcome::Pass(_)),
        "{control:?}"
    );
    let (engine, session, turn) = engine_with_turn(test_config());
    let hunks = [DiffHunk {
        old_start: 1,
        old_lines: 1,
        new_start: 1,
        new_lines: 2,
        lines: vec![
            DiffLine {
                kind: DiffLineKind::Added,
                text: "fn main() {".into(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                text: "}".into(),
            },
        ],
    }];
    let files = [StagedFile {
        path: Path::new("a.rs"),
        absolute_path: Path::new("/ws/a.rs"),
        before: Some(b"fn main() {\n}\n".as_ref()),
        after: Some(b"\xff\xfe".as_ref()),
        hunks: &hunks,
        pre_parse: None,
        post_parse: None,
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
            .all(|finding| finding.severity != FindingSeverity::Block),
        "{out:?}"
    );
    assert!(
        !out.iter().any(|finding| finding.text.contains("ploc")),
        "{out:?}"
    );
    let state = engine.state.lock().expect("engine state");
    let turn = state
        .sessions
        .get(&session)
        .expect("session")
        .turn
        .as_ref()
        .expect("turn");
    assert_eq!((turn.added, turn.deleted), (2, 0));
}

#[tokio::test]
async fn g1_reduction_ask() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "refactor the parser").await;
    let before_src = "fn main() {}\n";
    let after_src = "fn main() {\n let x = 1;\n}\n";
    let pre = parsed("/ws/a.rs", before_src).await;
    let post = parsed("/ws/a.rs", after_src).await;
    let mut lines = Vec::new();
    for index in 0..40 {
        lines.push(DiffLine {
            kind: DiffLineKind::Removed,
            text: format!("old {index}").into(),
        });
    }
    for index in 0..5 {
        lines.push(DiffLine {
            kind: DiffLineKind::Added,
            text: format!("new {index}").into(),
        });
    }
    let hunks = [DiffHunk {
        old_start: 1,
        old_lines: 40,
        new_start: 1,
        new_lines: 5,
        lines,
    }];
    let files = [StagedFile {
        path: Path::new("a.rs"),
        absolute_path: Path::new("/ws/a.rs"),
        before: Some(before_src.as_bytes()),
        after: Some(after_src.as_bytes()),
        hunks: &hunks,
        pre_parse: Some(pre),
        post_parse: Some(post),
    }];
    let batch = StagedBatch {
        session: rig.session,
        turn: turn_n(1),
        call: CallId::new("c1"),
        files: files.to_vec(),
    };
    let out = rig.engine.inspect(&batch);
    assert!(
        out.iter()
            .all(|finding| finding.severity != FindingSeverity::Block),
        "{out:?}"
    );
    rig.end(1).await;
    let report = rig
        .findings()
        .report
        .clone()
        .expect("reduction turn emits a report");
    assert!(
        report.contains("TURN CHANGE SUMMARY. Added 5, deleted 40, net -35, files 1, new files 0."),
        "{report}"
    );
    assert!(report.contains("deletion share 0.89"), "{report}");
    assert!(!report.contains("pure-addition"), "{report}");
    let pending = rig
        .begin(2, "follow up")
        .await
        .expect("next turn receives the model copy");
    assert_eq!(
        pending, *report,
        "model copy matches the stored turn report byte-identical"
    );
}

#[tokio::test]
async fn g6_never_builds() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "edit code").await;
    rig.touch(1, "src/a.rs");
    rig.end(1).await;
    let findings = rig.findings();
    assert!(findings.warnings.is_empty(), "{findings:?}");
    let report = findings.report.clone().unwrap_or_default();
    assert!(!report.contains("NEW WARNING NOTICE"), "{report}");
    assert!(!report.contains("NEW WARNING"), "{report}");
    rig.assert_no_stray_service_use();
}

#[tokio::test]
async fn g6_new_only() {
    let rig = Rig::open(test_config()).await;
    rig.begin(1, "build").await;
    rig.touch(1, "a.ml");
    assert_eq!(
        rig.call(1, &exec("c1", CARGO_BUILD)).await,
        ToolCallVerdict::Allow
    );
    let first = "File \"a.ml\", line 3, characters 4-5:\nWarning 26: unused variable x.\n";
    rig.result(1, "c1", "exec", true, first).await;
    rig.end(1).await;
    assert_eq!(rig.findings().warnings.len(), 1);
    rig.begin(2, "build again").await;
    rig.touch(2, "a.ml");
    rig.touch(2, "b.ml");
    assert_eq!(
        rig.call(2, &exec("c2", CARGO_BUILD)).await,
        ToolCallVerdict::Allow
    );
    let second = "File \"a.ml\", line 4, characters 4-5:\nWarning 26: unused variable x.\nFile \"b.ml\", line 7, characters 4-5:\nWarning 32: unused variable y.\n";
    rig.result(2, "c2", "exec", true, second).await;
    rig.end(2).await;
    let findings = rig.findings();
    let report = findings
        .report
        .as_deref()
        .expect("second turn emits a report");
    assert_eq!(report.matches("NEW WARNING NOTICE").count(), 1, "{report}");
    assert!(report.contains("b.ml:7"), "{report}");
    assert!(!report.contains("a.ml:4"), "{report}");
}

#[tokio::test]
async fn metrics_ocaml_golden() {
    let src = "let f x = x\n";
    let post = parsed("/ws/a.ml", src).await;
    let measured = metrics::measure(parse::Language::Ocaml, &post.tree, src.as_bytes());
    assert_eq!(measured.functions.len(), 1, "{measured:?}");
    let function = &measured.functions[0];
    assert_eq!(function.name.as_ref(), "f");
    assert_eq!(function.cyclomatic, 1, "{function:?}");
    assert_eq!(function.cognitive, 0, "{function:?}");
    assert_eq!(function.nesting, 0, "{function:?}");
    assert_eq!(function.ploc, 1, "{function:?}");
}

#[tokio::test]
async fn metrics_typescript_golden() {
    let src = "function f(x) {\nreturn x;\n}\n";
    let post = parsed("/ws/a.ts", src).await;
    let measured = metrics::measure(parse::Language::TypeScript, &post.tree, src.as_bytes());
    assert_eq!(measured.functions.len(), 1, "{measured:?}");
    let function = &measured.functions[0];
    assert_eq!(function.name.as_ref(), "f");
    assert_eq!(function.cyclomatic, 1, "{function:?}");
    assert_eq!(function.cognitive, 0, "{function:?}");
    assert_eq!(function.nesting, 0, "{function:?}");
    assert_eq!(function.ploc, 3, "{function:?}");
}

#[tokio::test]
async fn metrics_python_golden() {
    let src = "def f(x):\n    return x\n";
    let post = parsed("/ws/a.py", src).await;
    let measured = metrics::measure(parse::Language::Python, &post.tree, src.as_bytes());
    assert_eq!(measured.functions.len(), 1, "{measured:?}");
    let function = &measured.functions[0];
    assert_eq!(function.name.as_ref(), "f");
    assert_eq!(function.cyclomatic, 1, "{function:?}");
    assert_eq!(function.cognitive, 0, "{function:?}");
    assert_eq!(function.nesting, 0, "{function:?}");
    assert_eq!(function.ploc, 2, "{function:?}");
}

#[tokio::test]
async fn metrics_go_golden() {
    let src = "package main\nfunc f(x int) int {\nreturn x\n}\n";
    let post = parsed("/ws/a.go", src).await;
    let measured = metrics::measure(parse::Language::Go, &post.tree, src.as_bytes());
    assert_eq!(measured.functions.len(), 1, "{measured:?}");
    let function = &measured.functions[0];
    assert_eq!(function.name.as_ref(), "f");
    assert_eq!(function.cyclomatic, 1, "{function:?}");
    assert_eq!(function.cognitive, 0, "{function:?}");
    assert_eq!(function.nesting, 0, "{function:?}");
    assert_eq!(function.ploc, 3, "{function:?}");
}

#[tokio::test]
async fn metrics_c_golden() {
    let src = "int f(int x) {\nreturn x;\n}\n";
    let post = parsed("/ws/a.c", src).await;
    let measured = metrics::measure(parse::Language::C, &post.tree, src.as_bytes());
    assert_eq!(measured.functions.len(), 1, "{measured:?}");
    let function = &measured.functions[0];
    assert_eq!(function.name.as_ref(), "f");
    assert_eq!(function.cyclomatic, 1, "{function:?}");
    assert_eq!(function.cognitive, 0, "{function:?}");
    assert_eq!(function.nesting, 0, "{function:?}");
    assert_eq!(function.ploc, 3, "{function:?}");
}

#[tokio::test]
async fn metrics_cpp_golden() {
    let src = "int f(int x) {\nreturn x;\n}\n";
    let post = parsed("/ws/a.cpp", src).await;
    let measured = metrics::measure(parse::Language::Cpp, &post.tree, src.as_bytes());
    assert_eq!(measured.functions.len(), 1, "{measured:?}");
    let function = &measured.functions[0];
    assert_eq!(function.name.as_ref(), "f");
    assert_eq!(function.cyclomatic, 1, "{function:?}");
    assert_eq!(function.cognitive, 0, "{function:?}");
    assert_eq!(function.nesting, 0, "{function:?}");
    assert_eq!(function.ploc, 3, "{function:?}");
}
