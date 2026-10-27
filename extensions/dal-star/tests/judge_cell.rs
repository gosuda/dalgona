//! Eval-cell access to the `judge` globals: one granted call crossing the
//! host seam, one inject denial at the adapter prescreen.
//!
//! The dal-star rig answers `host.call` with scripted replies, so the
//! granted case pins a scripted verdict (`true`) crossing the real host seam
//! into a real eval cell. A fully wired `CellJudge` transport is not
//! reachable from this rig: the cell crosses `RecordingHost::call`
//! (`support/host.rs`), which answers from its scripted-replies map and never
//! routes through the production dispatch (`JudgeAsk` arm in
//! `engine/dal-agent/src/session/script/service.rs` → `ToolGate::judge_ask`
//! → `CellJudge::ask`). No `Services` handle here carries a judge backend
//! (`test_services` refuses every effect and its `set_judge_transport` is a
//! no-op), so instead of faking a judge this suite pins the prescreen denial
//! plus the evaluate-level error text.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]
#[path = "support/host.rs"]
pub mod host;
pub mod support;

use std::sync::Arc;

use dal_agent::ext::Extension;
use dal_agent::ext::tool::{Tool, ToolOutcome};
use dal_core::ext::{NativeOp, OpSet};
use dal_core::{Part, RawJson};
use dal_star::{CELL_LIMITS, eval_extension};
use host::{HostRecord, RecordingHost, completed_json, eval_args, native_op, tool_cx_approved};

async fn run_tool(
    tool: Arc<dyn Tool>,
    arguments: &str,
    script: dal_agent::ext::script::ScriptCx,
) -> ToolOutcome {
    let args = RawJson::parse(arguments).expect("tool arguments are JSON");
    tool.run(
        dal_agent::ext::tool::ToolCall::new("test-call", args),
        tool_cx_approved(script),
    )
    .await
}

fn named_tool(extension: &Extension, name: &str) -> Arc<dyn Tool> {
    extension
        .tools()
        .iter()
        .find(|(tool, _)| tool.name().as_str() == name)
        .map_or_else(
            || panic!("tool {name:?} is registered"),
            |(tool, _)| Arc::clone(tool),
        )
}

fn output_text(outcome: &ToolOutcome) -> &str {
    match outcome {
        ToolOutcome::Ok(output) => match &output.parts[0] {
            Part::Text { text } => text,
            other => panic!("expected text output, got {other:?}"),
        },
        ToolOutcome::Err(error) => panic!("expected successful tool outcome, got {error}"),
        other => panic!("expected completed tool outcome, got {other:?}"),
    }
}

fn failed_text(outcome: &ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Err(error) => error.to_string(),
        ToolOutcome::Ok(output) => panic!("expected failed tool outcome, got {output:?}"),
        other => panic!("expected failed tool outcome, got {other:?}"),
    }
}

fn call_records(host: &RecordingHost) -> Vec<HostRecord> {
    host.records()
        .into_iter()
        .filter(|record| matches!(record, HostRecord::Call { .. }))
        .collect()
}

/// Encodes an eval request with one explicit `uses` entry.
///
/// # Panics
///
/// Panics if the cell source or the operation id cannot be encoded as JSON.
fn eval_args_uses(code: &str, uses: &str) -> String {
    format!(
        "{{\"code\":{},\"uses\":[{}]}}",
        sonic_rs::to_string(code).expect("cell source is JSON"),
        sonic_rs::to_string(uses).expect("operation id is JSON"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn eval_cell_with_judge_inject_receives_scripted_verdict() {
    let extension = eval_extension(CELL_LIMITS).expect("eval extension");
    let tool = named_tool(&extension, "eval");
    let op = native_op(NativeOp::JudgeAsk);
    let host = RecordingHost::new(
        std::slice::from_ref(&extension),
        OpSet::parse(["judge.ask"]).expect("judge.ask is a native operation id"),
        [(op.clone(), completed_json(op.clone(), "true"))],
    );
    let outcome = run_tool(
        tool,
        &eval_args_uses("judge(\"is this true?\")", "judge.ask"),
        host.script_cx(),
    )
    .await;

    let text = output_text(&outcome);
    assert!(text.contains("\"status\":\"completed\""), "{text}");
    assert!(text.contains("\"value\":true"), "{text}");
    let calls = call_records(&host);
    assert!(
        matches!(
            calls.as_slice(),
            [HostRecord::Call { op: called, args, .. }]
                if called == &op && args.as_str().contains("is this true?")
        ),
        "the granted cell must cross the host seam exactly once: {calls:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn eval_cell_without_judge_inject_denies_before_dispatch() {
    let extension = eval_extension(CELL_LIMITS).expect("eval extension");
    let tool = named_tool(&extension, "eval");
    let host = RecordingHost::new(std::slice::from_ref(&extension), OpSet::EMPTY, []);
    let outcome = run_tool(
        tool,
        &eval_args("judge(\"is this true?\")"),
        host.script_cx(),
    )
    .await;

    let text = failed_text(&outcome);
    assert!(text.contains("\"status\":\"failed\""), "{text}");
    assert!(text.contains("\"code\":\"api_error\""), "{text}");
    // Exact adapter prescreen text (`{op}: {JUDGE_DENIED}` from
    // `OpAdapter::check_judge`), as JSON-escaped inside the envelope message:
    // `judge.ask: judge: denied (declare "judge" in the cell's inject)`.
    assert!(
        text.contains("judge.ask: judge: denied (declare \\\"judge\\\" in the cell's inject)"),
        "{text}"
    );
    assert!(
        call_records(&host).is_empty(),
        "the prescreen must fire before any host call: {text}"
    );
}
