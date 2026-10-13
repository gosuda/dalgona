//! Runtime adapter behavior over the scripted test host: calls, updates,
//! and shutdown ordering.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]
#[path = "support/host.rs"]
pub mod host;
pub mod support;

use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::Extension;
use dal_agent::ext::tool::{Tool, ToolOutcome};
use dal_core::ext::{NativeOp, OpSet, Service, ToolData, ViewNode};
use dal_core::{Part, RawJson};
use dal_star::{CELL_LIMITS, Limits, eval_extension};
use host::{HostRecord, RecordingHost, eval_args, failed, native_op, tool_cx_approved};
use support::{system_with_plugin, write_plugin};

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

#[tokio::test(flavor = "multi_thread")]
async fn eval_assignment_returns_null_and_print_is_diagnostic() {
    let extension = eval_extension(CELL_LIMITS).expect("eval extension");
    let tool = named_tool(&extension, "eval");
    let extensions = [extension];
    let host = RecordingHost::new(&extensions, OpSet::EMPTY, []);
    let outcome = run_tool(
        tool,
        &eval_args("print(\"diagnostic\")\nanswer = 42"),
        host.script_cx(),
    )
    .await;

    let text = output_text(&outcome);
    assert!(text.contains("\"status\":\"completed\""), "{text}");
    assert!(text.contains("\"value\":null"), "{text}");
    assert!(
        text.contains("\"prints\":{\"lines\":[\"diagnostic\"]"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_native_failure_stops_eval_with_the_operation_code() {
    let extension = eval_extension(CELL_LIMITS).expect("eval extension");
    let op = native_op(NativeOp::EnvRead);
    let reply = failed(
        op.clone(),
        dal_agent::ext::script::FailureCode::Domain("quota".into()),
        "over",
    );
    let host = RecordingHost::new(
        std::slice::from_ref(&extension),
        OpSet::parse(["env.read"]).expect("native operation id"),
        [(op.clone(), reply)],
    );
    let outcome = run_tool(
        named_tool(&extension, "eval"),
        &eval_args("ctx.env.read(key = \"credential\")"),
        host.script_cx(),
    )
    .await;

    let text = failed_text(&outcome);
    assert!(text.contains("\"status\":\"failed\""), "{text}");
    assert!(text.contains("\"code\":\"quota\""), "{text}");
    let calls = call_records(&host);
    assert!(matches!(
        calls.as_slice(),
        [HostRecord::Call { caller, op: called, args, .. }]
            if caller.as_ref() == "test"
                && called == &op
                && args.as_str().contains("\"key\":\"credential\"")
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn try_call_recovers_one_native_failure() {
    let extension = eval_extension(CELL_LIMITS).expect("eval extension");
    let op = native_op(NativeOp::EnvRead);
    let reply = failed(
        op.clone(),
        dal_agent::ext::script::FailureCode::Domain("quota".into()),
        "over",
    );
    let host = RecordingHost::new(
        std::slice::from_ref(&extension),
        OpSet::parse(["env.read"]).expect("native operation id"),
        [(op.clone(), reply)],
    );
    let outcome = run_tool(
        named_tool(&extension, "eval"),
        &eval_args("result = ctx.try_call(ctx.env.read, key = \"credential\")\nresult.ok"),
        host.script_cx(),
    )
    .await;

    let text = output_text(&outcome);
    assert!(text.contains("\"status\":\"completed\""), "{text}");
    assert!(text.contains("\"value\":false"), "{text}");
    let calls = call_records(&host);
    assert!(matches!(
        calls.as_slice(),
        [HostRecord::Call { caller, op: called, .. }]
            if caller.as_ref() == "test" && called == &op
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_domain_error_is_a_failed_tool_outcome() {
    let source = r#"load("@dal/v1", "dal")
def fail(ctx, args):
    return dal.err("quota", "over")

fail_tool = dal.tool(description = "Fail with a domain code.", input = dal.schema(), run = fail)
plugin = dal.plugin(name = "quota", version = "0.1.0", tools = {"fail": fail_tool})
"#;
    let (_data, system) = system_with_plugin("quota", source);
    let extensions = system.extensions().expect("plugin converts to extension");
    let tool = named_tool(&extensions[0], "quota__fail");
    let host = RecordingHost::new(&extensions, OpSet::EMPTY, []);
    let outcome = run_tool(tool, "{}", host.script_cx()).await;

    let text = failed_text(&outcome);
    assert!(text.contains("quota"), "{text}");
    assert!(text.contains("over"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn false_ok_field_remains_successful_plugin_data() {
    let source = r#"load("@dal/v1", "dal")
def return_data(ctx, args):
    return {"ok": False}

data_tool = dal.tool(description = "Return data.", input = dal.schema(), run = return_data)
plugin = dal.plugin(name = "plain", version = "0.1.0", tools = {"data": data_tool})
"#;
    let (_data, system) = system_with_plugin("plain", source);
    let extensions = system.extensions().expect("plugin converts to extension");
    let tool = named_tool(&extensions[0], "plain__data");
    let host = RecordingHost::new(&extensions, OpSet::EMPTY, []);
    let outcome = run_tool(tool, "{}", host.script_cx()).await;

    assert_eq!(output_text(&outcome), "{\"ok\":false}");
}

#[test]
fn failed_reload_keeps_the_published_generation() {
    let source = r#"load("@dal/v1", "dal")
plugin = dal.plugin(name = "stable", version = "0.1.0")
"#;
    let (data, system) = system_with_plugin("stable", source);
    let before = system.snapshot().expect("published generation");
    write_plugin(data.path(), "stable", "plugin =\n");

    assert!(system.reload().is_err(), "broken replacement must fail");
    let after = system
        .snapshot()
        .expect("prior generation remains readable");
    assert_eq!(after.id, before.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn scope_policy_and_usd_budget_reach_the_host_scope_spec() {
    let source = r#"load("@dal/v1", "dal")
def open_scope(ctx, args):
    ctx.scope(limit = 8, on_error = "settle", usd = 0.40)
    return None

open_scope_tool = dal.tool(description = "Open a bounded scope.", input = dal.schema(), run = open_scope)
plugin = dal.plugin(name = "scopepolicy", version = "0.1.0", tools = {"open": open_scope_tool})
"#;
    let (_data, system) = system_with_plugin("scopepolicy", source);
    let extensions = system.extensions().expect("plugin converts");
    let host = RecordingHost::new(&extensions, OpSet::EMPTY, []);
    let outcome = run_tool(
        named_tool(&extensions[0], "scopepolicy__open"),
        "{}",
        host.script_cx(),
    )
    .await;
    assert!(matches!(outcome, ToolOutcome::Ok(_)), "{outcome:?}");
    let spec = host
        .records()
        .into_iter()
        .find_map(|record| match record {
            HostRecord::OpenScope { spec, .. } => Some(spec),
            _ => None,
        })
        .expect("scope spec reaches the host");
    assert_eq!(spec.limit, 8);
    assert_eq!(spec.on_error, dal_core::OnError::Settle);
    assert_eq!(spec.budget.usd, Some(0.40));
    assert_eq!(spec.budget.input_tokens, None);
}

#[test]
fn model_declaration_converts_with_public_route_and_capabilities() {
    let source = r#"load("@dal/v1", "dal")
def infer(ctx, request):
    return None

caps = {
    "context_window": 8192,
    "thinking": ["minimal", "high"],
    "tool_use": True,
    "image_input": False,
    "custom_grammar": True,
}
model = dal.model(id = "dalgona/fusion", caps = caps, run = infer, uses = ["models.forward"])
plugin = dal.plugin(name = "modelled", version = "0.1.0", inject = ["infer"], models = {"fusion": model})
"#;
    let (_data, system) = system_with_plugin("modelled", source);
    let extensions = system.extensions().expect("model conversion succeeds");
    let models = extensions[0].models();

    assert!(extensions[0].inject().contains(Service::Infer));
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id.as_str(), "dalgona/fusion");
    assert_eq!(
        models[0]
            .export
            .as_ref()
            .map(|export| export.local.as_str()),
        Some("fusion")
    );
    assert_eq!(
        models[0].handler.uses(),
        OpSet::parse(["models.forward"]).expect("declared model operation")
    );
    assert_eq!(
        models[0].caps,
        dal_core::Caps {
            context_window: Some(8192),
            thinking: vec![
                dal_core::ThinkingLevel::Minimal,
                dal_core::ThinkingLevel::High
            ]
            .into_boxed_slice(),
            tool_use: true,
            image_input: false,
            custom_grammar: true,
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn output_table_becomes_typed_display_data() {
    let source = r#"load("@dal/v1", "dal")
def show(ctx, args):
    return dal.output(value = 1, view = {"type": "table", "columns": ["a"], "rows": [[1]]})

show_tool = dal.tool(description = "Show a table.", input = dal.schema(), run = show)
plugin = dal.plugin(name = "tables", version = "0.1.0", tools = {"show": show_tool})
"#;
    let (_data, system) = system_with_plugin("tables", source);
    let extensions = system.extensions().expect("plugin converts to extension");
    let tool = named_tool(&extensions[0], "tables__show");
    let host = RecordingHost::new(&extensions, OpSet::EMPTY, []);
    let outcome = run_tool(tool, "{}", host.script_cx()).await;

    let ToolOutcome::Ok(output) = outcome else {
        panic!("table output must succeed: {outcome:?}");
    };
    assert!(matches!(
        output.data,
        Some(ToolData::Display(ViewNode::Table { ref columns, ref rows }))
            if columns.iter().map(AsRef::as_ref).collect::<Vec<_>>() == ["a"]
                && rows.len() == 1
                && rows[0].len() == 1
                && rows[0][0].as_str() == "1"
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn ragged_output_table_fails_the_tool_call() {
    let source = r#"load("@dal/v1", "dal")
def show(ctx, args):
    return dal.output(value = 1, view = {"type": "table", "columns": ["a"], "rows": [[1, 2]]})

show_tool = dal.tool(description = "Show an invalid table.", input = dal.schema(), run = show)
plugin = dal.plugin(name = "ragged", version = "0.1.0", tools = {"show": show_tool})
"#;
    let (_data, system) = system_with_plugin("ragged", source);
    let extensions = system.extensions().expect("plugin converts to extension");
    let tool = named_tool(&extensions[0], "ragged__show");
    let host = RecordingHost::new(&extensions, OpSet::EMPTY, []);
    let outcome = run_tool(tool, "{}", host.script_cx()).await;

    let message = failed_text(&outcome);
    assert!(
        message.contains("table row has 2 cells; expected 1"),
        "{message}"
    );
}
#[tokio::test(flavor = "multi_thread")]
async fn long_eval_loop_times_out_and_finishes_its_invocation() {
    let limits = Limits {
        ticks: u64::MAX,
        ..CELL_LIMITS
    };
    let extension = eval_extension(limits).expect("eval extension");
    let tool = named_tool(&extension, "eval");
    let extensions = [extension];
    let host =
        RecordingHost::with_budget(&extensions, OpSet::EMPTY, [], Duration::from_millis(250));
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        run_tool(
            tool,
            &eval_args("for _ in range(1000000000):\n    pass"),
            host.script_cx(),
        ),
    )
    .await
    .expect("the timed-out interpreter worker must return");

    let text = failed_text(&outcome);
    assert!(text.contains("\"status\":\"limit_exceeded\""), "{text}");
    assert!(text.contains("\"code\":\"timeout\""), "{text}");
    assert!(
        host.records()
            .iter()
            .any(|record| matches!(record, HostRecord::Finish { .. })),
        "the invocation must finish after the worker returns"
    );
}
