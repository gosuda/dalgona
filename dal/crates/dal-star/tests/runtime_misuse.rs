//! Adversarial violation tests for the Starlark runtime adapter: malformed
//! eval requests, scripts that fail mid-run, invalid `dal.*` call arguments,
//! capability and scope misuse, and host outcomes the script must not be
//! able to swallow. Every case goes through the public tool surface over the
//! recording host.

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
use dal_agent::ext::script::{FailureCode, HostTerminal, OpOutcome};
use dal_agent::ext::tool::{Tool, ToolCall, ToolCx, ToolOutcome};
use dal_core::ext::{NativeOp, OpId, OpSet};
use dal_core::{Part, RawJson};
use dal_star::{CELL_LIMITS, eval_extension};
use host::{
    Fault, HostRecord, RecordingHost, completed_json, eval_args, failed, native_op, test_services,
    tool_cx,
};
use support::system_with_plugin;

// --- harness ----------------------------------------------------------------

#[derive(serde::Deserialize, Debug)]
struct Envelope {
    status: String,
    error: Option<Failure>,
}

#[derive(serde::Deserialize, Debug)]
struct Failure {
    code: String,
    message: String,
}

struct Run {
    host: Arc<RecordingHost>,
    outcome: ToolOutcome,
}

impl Run {
    fn records(&self) -> Vec<HostRecord> {
        self.host.records()
    }

    fn count(&self, matching: fn(&HostRecord) -> bool) -> usize {
        self.records()
            .iter()
            .filter(|record| matching(record))
            .count()
    }

    fn calls(&self) -> usize {
        self.count(|record| matches!(record, HostRecord::Call { .. }))
    }

    fn begins(&self) -> usize {
        self.count(|record| matches!(record, HostRecord::Begin { .. }))
    }

    fn finishes(&self) -> usize {
        self.count(|record| matches!(record, HostRecord::Finish { .. }))
    }

    /// The text of a failed tool outcome.
    fn failure_text(&self) -> String {
        match &self.outcome {
            ToolOutcome::Err(error) => error.to_string(),
            other => panic!("expected a failed tool outcome, got {other:?}"),
        }
    }

    /// The eval envelope, for a completed or a failed cell.
    fn envelope(&self) -> Envelope {
        let text = match &self.outcome {
            ToolOutcome::Err(error) => error.to_string(),
            ToolOutcome::Ok(output) => match &output.parts[0] {
                Part::Text { text } => text.to_string(),
                other => panic!("expected text output, got {other:?}"),
            },
            other => panic!("expected an envelope, got {other:?}"),
        };
        sonic_rs::from_str(&text)
            .unwrap_or_else(|error| panic!("not an envelope ({error}): {text}"))
    }

    fn completed_text(&self) -> String {
        match &self.outcome {
            ToolOutcome::Ok(output) => match &output.parts[0] {
                Part::Text { text } => text.to_string(),
                other => panic!("expected text output, got {other:?}"),
            },
            other => panic!("expected a completed cell, got {other:?}"),
        }
    }

    /// Asserts the cell failed with `status` and `code`, and returns its message.
    fn assert_failed(&self, status: &str, code: &str) -> String {
        let envelope = self.envelope();
        assert_eq!(envelope.status, status, "{envelope:?}");
        let failure = envelope.error.expect("a failed envelope carries an error");
        assert_eq!(failure.code, code, "{}", failure.message);
        failure.message
    }
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

async fn eval_raw(arguments: &str, allowed: &[&str], replies: Vec<(OpId, OpOutcome)>) -> Run {
    let extension = eval_extension(CELL_LIMITS).expect("eval extension");
    let tool = named_tool(&extension, "eval");
    let extensions = [extension];
    let allowed = OpSet::parse(allowed.iter().copied()).expect("allowed operations");
    let host = RecordingHost::new(&extensions, allowed, replies);
    eval_on(tool, host, arguments).await
}

async fn eval_on(tool: Arc<dyn Tool>, host: Arc<RecordingHost>, arguments: &str) -> Run {
    let args = RawJson::parse(arguments).expect("tool arguments are JSON");
    let outcome = tool
        .run(
            ToolCall::new("adversarial", args),
            tool_cx(host.script_cx()),
        )
        .await;
    Run { host, outcome }
}

async fn eval_with(code: &str, allowed: &[&str], replies: Vec<(OpId, OpOutcome)>) -> Run {
    eval_raw(&eval_args(code), allowed, replies).await
}

async fn eval(code: &str) -> Run {
    eval_with(code, &[], Vec::new()).await
}

// --- eval request surface ---------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn unknown_request_field_is_rejected_before_any_invocation() {
    let run = eval_raw("{\"code\":\"1\",\"bogus\":2}", &[], Vec::new()).await;
    assert!(
        run.failure_text()
            .contains("unknown eval request field `bogus`")
    );
    assert_eq!(run.begins(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn non_object_requests_are_rejected() {
    for request in ["[]", "\"x\"", "null", "5", "true"] {
        let run = eval_raw(request, &[], Vec::new()).await;
        assert!(
            run.failure_text().contains("must be an object"),
            "{request}: {}",
            run.failure_text()
        );
        assert_eq!(run.begins(), 0);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn code_must_be_a_string() {
    for request in [
        "{}",
        "{\"code\":5}",
        "{\"code\":null}",
        "{\"code\":[\"1\"]}",
    ] {
        let run = eval_raw(request, &[], Vec::new()).await;
        assert!(
            run.failure_text().contains("`code` must be a string"),
            "{request}: {}",
            run.failure_text()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn uses_must_be_a_list_of_known_distinct_exact_ids() {
    let cases = [
        ("\"env.read\"", "must be a list of operation ids"),
        ("[1]", "must be a list of operation ids"),
        ("{}", "must be a list of operation ids"),
        ("[\"nope.op\"]", "unknown operation"),
        ("[\"env.read\",\"env.read\"]", "appears twice"),
        ("[\"env.*\"]", "wildcard"),
        ("[\"\"]", "unknown operation"),
    ];
    for (uses, needle) in cases {
        let run = eval_raw(
            &format!("{{\"code\":\"1\",\"uses\":{uses}}}"),
            &[],
            Vec::new(),
        )
        .await;
        assert!(
            run.failure_text().contains(needle),
            "uses {uses}: {}",
            run.failure_text()
        );
        assert_eq!(run.begins(), 0, "uses {uses} must not mint an invocation");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn uses_list_boundary_is_sixty_four_entries() {
    let ids = |count: usize| {
        (0..count)
            .map(|index| format!("\"tools.p.t{index}\""))
            .collect::<Vec<_>>()
            .join(",")
    };
    let over = eval_raw(
        &format!("{{\"code\":\"1\",\"uses\":[{}]}}", ids(65)),
        &[],
        Vec::new(),
    )
    .await;
    assert!(
        over.failure_text()
            .contains("65 uses entries exceeds the limit of 64"),
        "{}",
        over.failure_text()
    );
    // At the cap the list parses; the host then refuses authority the
    // environment never granted, which is a different failure.
    for count in [63, 64] {
        let at = eval_raw(
            &format!("{{\"code\":\"1\",\"uses\":[{}]}}", ids(count)),
            &[],
            Vec::new(),
        )
        .await;
        assert!(
            !at.failure_text().contains("exceeds the limit"),
            "{count} entries: {}",
            at.failure_text()
        );
    }
}

fn padded_cell(total: usize) -> String {
    let mut code = String::from("1\n#");
    while code.len() < total {
        code.push('a');
    }
    code
}

#[tokio::test(flavor = "multi_thread")]
async fn code_size_boundary_is_max_code() {
    const MAX_CODE: usize = 256 << 10;
    for total in [MAX_CODE - 1, MAX_CODE] {
        let run = eval(&padded_cell(total)).await;
        assert_eq!(run.envelope().status, "completed", "{total} bytes");
    }
    let run = eval(&padded_cell(MAX_CODE + 1)).await;
    assert!(
        run.failure_text().contains("exceeds 262144 bytes"),
        "{}",
        run.failure_text()
    );
    assert_eq!(run.begins(), 0);
}

fn request_with_data_len(total: usize) -> String {
    let frame = "{\"code\":\"1\",\"uses\":[],\"data\":\"\"}".len();
    format!(
        "{{\"code\":\"1\",\"uses\":[],\"data\":\"{}\"}}",
        "d".repeat(total - frame)
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn request_payload_boundary_is_one_mebibyte() {
    const MAX_DATA: usize = 1 << 20;
    for total in [MAX_DATA - 1, MAX_DATA] {
        let run = eval_raw(&request_with_data_len(total), &[], Vec::new()).await;
        assert_eq!(run.envelope().status, "completed", "{total} bytes");
    }
    let run = eval_raw(&request_with_data_len(MAX_DATA + 1), &[], Vec::new()).await;
    assert!(
        run.failure_text().contains("payload exceeds"),
        "{}",
        run.failure_text()
    );
    assert_eq!(run.begins(), 0);
}

fn request_with_data_depth(depth: usize) -> String {
    format!(
        "{{\"code\":\"1\",\"uses\":[],\"data\":{}{}}}",
        "[".repeat(depth),
        "]".repeat(depth)
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn request_data_nesting_boundary_is_sixty_four_levels() {
    // The request object is level 0, so `data` nested 64 deep sits at level 64.
    for depth in [63, 64] {
        let run = eval_raw(&request_with_data_depth(depth), &[], Vec::new()).await;
        assert_eq!(run.envelope().status, "completed", "depth {depth}");
    }
    let run = eval_raw(&request_with_data_depth(65), &[], Vec::new()).await;
    assert!(
        run.failure_text().contains("nests deeper than 64"),
        "{}",
        run.failure_text()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_request_json_is_a_typed_rejection() {
    for request in [
        "{\"code\":\"1\",\"code\":\"2\"}",
        "{\"code\":\"1\",\"data\":9007199254740992}",
        "{\"code\":\"1\",\"data\":-9007199254740992}",
    ] {
        let run = eval_raw(request, &[], Vec::new()).await;
        let text = run.failure_text();
        assert!(
            text.contains("duplicate key") || text.contains("53-bit"),
            "{request}: {text}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_53_bit_integer_bounds_are_inclusive() {
    for request in [
        "{\"code\":\"data\",\"uses\":[],\"data\":9007199254740991}",
        "{\"code\":\"data\",\"uses\":[],\"data\":-9007199254740991}",
    ] {
        let run = eval_raw(request, &[], Vec::new()).await;
        assert_eq!(run.envelope().status, "completed", "{request}");
    }
}

// --- scripts that fail mid-run ----------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn syntax_error_in_a_cell_names_the_cell_and_mints_no_invocation() {
    let run = eval("def (").await;
    assert!(
        run.failure_text().contains("cell.star"),
        "{}",
        run.failure_text()
    );
    assert_eq!(run.begins(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_error_is_a_failed_cell_with_a_finished_invocation() {
    let run = eval("x = undefined_name + 1").await;
    let message = run.assert_failed("failed", "eval_error");
    assert!(message.contains("undefined_name"), "{message}");
    assert_eq!((run.begins(), run.finishes()), (1, 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_fail_keeps_its_message() {
    let run = eval("fail(\"boom-marker\")").await;
    let message = run.assert_failed("failed", "eval_error");
    assert!(message.contains("boom-marker"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn error_after_a_host_call_still_finishes_the_invocation_once() {
    let op = native_op(NativeOp::EnvRead);
    let reply = failed(op.clone(), FailureCode::Unavailable, "gone");
    let run = eval_with(
        "ctx.try_call(ctx.env.read, key = \"k\")\nfail(\"after\")",
        &["env.read"],
        vec![(op, reply)],
    )
    .await;
    run.assert_failed("failed", "eval_error");
    assert_eq!(run.calls(), 1);
    assert_eq!((run.begins(), run.finishes()), (1, 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_attributes_are_ordinary_script_errors_not_host_calls() {
    for code in [
        "ctx.bogus",
        "ctx.tools.bogus",
        "ctx.env.bogus",
        "ctx.models.bogus(1)",
        "scope = ctx.scope(limit = 1)\nscope.bogus",
    ] {
        let run = eval(code).await;
        run.assert_failed("failed", "eval_error");
        assert_eq!(run.calls(), 0, "{code}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn results_outside_the_transport_set_fail_the_cell() {
    for code in [
        "lambda: 1",
        "{1: 2}",
        "{(1, 2): 3}",
        "float(\"inf\")",
        "1 << 60",
        "ctx",
        "ctx.env.read",
    ] {
        let run = eval(code).await;
        run.assert_failed("failed", "eval_error");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn returned_values_beyond_the_transport_depth_fail_the_cell() {
    let build = |depth: usize| format!("x = 1\nfor _ in range({depth}):\n    x = [x]\nx");
    for depth in [63, 64] {
        let ok = eval(&build(depth)).await;
        assert_eq!(ok.envelope().status, "completed", "depth {depth}");
    }
    let over = eval(&build(65)).await;
    let message = over.assert_failed("failed", "eval_error");
    assert!(message.contains("nests deeper"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn print_output_boundary_is_sixty_four_kibibytes() {
    const MAX_PRINTS: usize = 64 << 10;
    let at = eval(&format!("print(\"p\" * {MAX_PRINTS})")).await;
    assert!(
        at.completed_text().contains("\"truncated\":false"),
        "{}",
        at.completed_text()
    );
    let under = eval(&format!("print(\"p\" * {})", MAX_PRINTS - 1)).await;
    assert!(under.completed_text().contains("\"truncated\":false"));
    let over = eval(&format!("print(\"p\" * {})", MAX_PRINTS + 1)).await;
    let text = over.completed_text();
    assert!(text.contains("\"truncated\":true"), "{text}");
    assert!(text.contains("prints truncated at 64 KiB"), "{text}");
    assert!(
        !text.contains("pppppppp"),
        "the oversized line must not be retained"
    );
}

// --- invalid arguments to dal.* calls ---------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn misshapen_native_calls_are_api_errors_that_never_reach_the_host() {
    let cases = [
        ("ctx.env.read(\"positional\")", "named arguments only"),
        ("ctx.models.infer(1, 2)", "at most one positional request"),
        (
            "ctx.models.infer({\"x\": 1}, request = {})",
            "both positionally and by name",
        ),
        (
            "ctx.models.infer(request = {\"bogus\": 1})",
            "unknown field",
        ),
        ("ctx.models.infer(request = 5)", "request"),
        ("ctx.env.read(key = lambda: 1)", "function"),
        ("ctx.env.read(key = float(\"inf\"))", "non-finite"),
        ("ctx.env.read(key = 1 << 60)", "53-bit"),
        ("ctx.env.read(key = {1: 2})", "dict key"),
    ];
    for (code, needle) in cases {
        let run = eval_with(code, &["env.read", "models.infer"], Vec::new()).await;
        let message = run.assert_failed("failed", "api_error");
        assert!(message.contains(needle), "{code}: {message}");
        assert_eq!(run.calls(), 0, "{code} must not reach the host");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn arguments_nested_beyond_the_transport_depth_are_api_errors() {
    let build = |depth: usize| {
        format!("x = 1\nfor _ in range({depth}):\n    x = [x]\nctx.env.read(key = x)")
    };
    let over = eval_with(&build(65), &["env.read"], Vec::new()).await;
    let message = over.assert_failed("failed", "api_error");
    assert!(message.contains("nests deeper"), "{message}");
    assert_eq!(over.calls(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn try_call_rejects_non_adapters_and_scheduled_adapters() {
    let cases = [
        ("ctx.try_call()", "pass the operation adapter first"),
        ("ctx.try_call(5)", "host-issued operation adapter"),
        ("ctx.try_call(ctx.tools)", "host-issued operation adapter"),
        (
            "scope = ctx.scope(limit = 1)\nctx.try_call(scope.tools.read, path = \"a\")",
            "scheduled scope adapters cannot be retried",
        ),
        (
            "ctx.try_call(ctx.tools[\"a.b\"])",
            "native operation adapters only",
        ),
    ];
    for (code, needle) in cases {
        let run = eval_with(code, &["tools.read"], Vec::new()).await;
        let message = run.assert_failed("failed", "api_error");
        assert!(message.contains(needle), "{code}: {message}");
        assert_eq!(run.calls(), 0, "{code}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn try_call_cannot_recover_script_api_misuse() {
    let run = eval_with(
        "r = ctx.try_call(ctx.env.read, \"positional\")\nr.ok",
        &["env.read"],
        Vec::new(),
    )
    .await;
    run.assert_failed("failed", "api_error");
    assert_eq!(run.calls(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn export_keys_are_validated_before_minting_an_adapter() {
    let cases = [
        ("ctx.tools[5]", "export keys must be strings"),
        ("ctx.tools[\"nodots\"]", "export ids spell"),
        ("ctx.tools[\"a.b.c\"]", "export ids spell"),
        ("ctx.tools[\"A.b\"]", "tools"),
        ("ctx.tools[\"a.\"]", "tools"),
        (
            "ctx.net[\"a.b\"]",
            "only tools/models facades serve exports",
        ),
        (
            "scope = ctx.scope(limit = 1)\nscope.tools[\"a.b\"]",
            "cannot schedule scripted exports",
        ),
    ];
    for (code, needle) in cases {
        let run = eval(code).await;
        let message = run.assert_failed("failed", "api_error");
        assert!(message.contains(needle), "{code}: {message}");
        assert_eq!(run.calls(), 0);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ctx_methods_reject_bad_arguments() {
    let cases = [
        (
            "ctx.describe(\"bogus.op\")",
            "api_error",
            "unknown operation",
        ),
        ("ctx.describe(\"\")", "api_error", "unknown operation"),
        ("ctx.describe(5)", "eval_error", ""),
        ("ctx.can(5)", "eval_error", ""),
        ("ctx.scope()", "eval_error", ""),
        (
            "ctx.show({\"path\": \"a\", \"line\": 1, \"text\": \"t\"})",
            "api_error",
            "intact host-issued",
        ),
        ("ctx.show(5)", "api_error", "intact host-issued"),
    ];
    for (code, expected, needle) in cases {
        let run = eval(code).await;
        let message = run.assert_failed("failed", expected);
        assert!(message.contains(needle), "{code}: {message}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn can_answers_false_for_unknown_ids_instead_of_raising() {
    let source = plugin_with(
        "def run(ctx, args):\n    return [ctx.can(\"bogus\"), ctx.can(\"\"), ctx.can(\"env.read\"), ctx.can(\"tools.exec\")]\n",
        "",
        ", uses = [\"env.read\"]",
    );
    let run = run_plugin_tool(&source, "{}").await;
    assert_eq!(ok_text(&run.outcome), "[false,false,true,false]");
}

#[tokio::test(flavor = "multi_thread")]
async fn adopt_refusal_is_a_recoverable_boundary_failure_code() {
    let run = eval("ctx.adopt(\"missing-ref\")").await;
    run.assert_failed("failed", "observation_unavailable");
}

// --- scope misuse -----------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn scope_limit_boundary_is_one_to_sixty_four() {
    for limit in [1, 64] {
        let run = eval(&format!("ctx.scope(limit = {limit})\nNone")).await;
        assert_eq!(run.envelope().status, "completed", "limit {limit}");
        let spec = run
            .records()
            .into_iter()
            .find_map(|record| match record {
                HostRecord::OpenScope { spec, .. } => Some(spec),
                _ => None,
            })
            .expect("a valid limit reaches the host");
        assert_eq!(u32::from(spec.limit), limit);
    }
    for limit in ["0", "-1", "65", "65537", "2147483647"] {
        let run = eval(&format!("ctx.scope(limit = {limit})")).await;
        let message = run.assert_failed("failed", "api_error");
        assert!(
            message.contains("limit must be in 1..=64"),
            "limit {limit}: {message}"
        );
        assert!(
            !run.records()
                .iter()
                .any(|record| matches!(record, HostRecord::OpenScope { .. })),
            "limit {limit} must not open a host scope"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn scope_policy_and_budget_fields_are_strictly_decoded() {
    let cases = [
        ("on_error = \"retry\"", "unknown on_error policy"),
        ("budget = 5", "budget must be an object"),
        ("budget = {\"requests\": -1}", "non-negative"),
        ("budget = {\"requests\": 1.5}", "must be an integer"),
        ("budget = {\"requests\": True}", "must be an integer"),
        ("budget = {\"input_tokens\": \"9\"}", "must be an integer"),
        ("budget = {\"bogus\": 1}", "unknown budget field"),
        ("usd = 0", "positive and finite"),
        ("usd = -0.5", "positive and finite"),
        ("usd = float(\"inf\")", "invalid usd budget"),
        ("usd = \"1\"", "usd"),
        ("budget = {\"wall\": 0}", "positive and finite"),
        ("budget = {\"wall\": -3}", "positive and finite"),
        ("budget = {\"wall\": 1e300}", "out of range"),
        ("budget = {\"wall\": \"1\"}", "number of seconds"),
        ("usd = 0.5, budget = {\"usd\": 0.5}", "supplied both"),
    ];
    for (fields, needle) in cases {
        let run = eval(&format!("ctx.scope(limit = 1, {fields})")).await;
        let message = run.assert_failed("failed", "api_error");
        assert!(message.contains(needle), "{fields}: {message}");
        assert!(
            !run.records()
                .iter()
                .any(|record| matches!(record, HostRecord::OpenScope { .. })),
            "{fields} must not open a host scope"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn smallest_positive_usd_and_zero_counts_are_accepted() {
    let run = eval("ctx.scope(limit = 1, usd = 1e-9, budget = {\"requests\": 0})\nNone").await;
    assert_eq!(run.envelope().status, "completed");
}

#[tokio::test(flavor = "multi_thread")]
async fn submitting_through_a_sealed_scope_is_an_api_error_with_no_submit() {
    let cases = [
        "s = ctx.scope(limit = 2)\ns.settle()\ns.tools.read(path = \"a\")",
        "s = ctx.scope(limit = 2)\ns.all()\ns.net.fetch(url = \"https://x.test\")",
        "s = ctx.scope(limit = 2)\nf = s.tools.read\ns.settle()\nf(path = \"a\")",
        "s = ctx.scope(limit = 2)\ns.all()\nf = s.net.fetch\nf(url = \"https://x.test\")",
    ];
    for code in cases {
        let run = eval(code).await;
        let message = run.assert_failed("failed", "api_error");
        assert!(message.contains("sealed"), "{code}: {message}");
        assert!(
            !run.records()
                .iter()
                .any(|record| matches!(record, HostRecord::Submit { .. })),
            "{code}: a sealed scope must not submit"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn submitting_before_sealing_reaches_the_host_once_per_call() {
    let run = eval(
        "s = ctx.scope(limit = 2)\ns.tools.read(path = \"a\")\ns.tools.read(path = \"b\")\nNone",
    )
    .await;
    assert_eq!(run.envelope().status, "completed");
    assert_eq!(
        run.count(|record| matches!(record, HostRecord::Submit { .. })),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn waiting_on_a_task_the_host_never_answers_is_a_typed_error() {
    let run = eval("s = ctx.scope(limit = 1)\nt = s.tools.read(path = \"a\")\nt.wait()").await;
    let message = run.assert_failed("failed", "api_error");
    assert!(
        message.contains("the host returned no outcome"),
        "{message}"
    );
    let run = eval("s = ctx.scope(limit = 1)\nt = s.tools.read(path = \"a\")\nt.settle()").await;
    run.assert_failed("failed", "api_error");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scope_handle_cannot_leave_the_invocation_and_the_unsealed_scope_is_cancelled() {
    let run = eval("ctx.scope(limit = 1)").await;
    run.assert_failed("failed", "eval_error");
    assert!(
        run.records()
            .iter()
            .any(|record| matches!(record, HostRecord::Cancel { .. })),
        "dropping an unsealed scope must ask the host to cancel its work"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sealed_scope_is_not_cancelled_on_drop() {
    let run = eval("s = ctx.scope(limit = 1)\ns.all()\nNone").await;
    assert_eq!(run.envelope().status, "completed");
    assert!(
        !run.records()
            .iter()
            .any(|record| matches!(record, HostRecord::Cancel { .. })),
        "a sealed scope has nothing left to cancel"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn task_and_scope_handles_cannot_be_returned_as_data() {
    for code in [
        "s = ctx.scope(limit = 1)\nt = s.tools.read(path = \"a\")\ns.all()\nt",
        "s = ctx.scope(limit = 1)\ns.all()\ns",
        "s = ctx.scope(limit = 1)\ns.settle()\ns.tools",
    ] {
        let run = eval(code).await;
        run.assert_failed("failed", "eval_error");
    }
}

// --- authority --------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn non_pure_eval_fails_closed_before_minting_an_invocation() {
    let run = eval_raw(
        "{\"code\":\"1\",\"uses\":[\"tools.exec\"]}",
        &[],
        Vec::new(),
    )
    .await;
    assert!(
        run.failure_text().contains("no front end"),
        "{}",
        run.failure_text()
    );
    assert_eq!(
        run.begins(),
        0,
        "a refused entry must not mint an invocation"
    );
    assert_eq!(run.calls(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_handler_sees_exactly_its_declared_ceiling() {
    let source = plugin_with(
        "def run(ctx, args):\n    return [ctx.describe(\"env.read\")[\"allowed\"], ctx.describe(\"net.fetch\")[\"allowed\"], ctx.describe(\"tools.exec\")[\"allowed\"], ctx.describe()[\"phase\"]]\n",
        "",
        ", uses = [\"env.read\", \"net.fetch\"]",
    );
    let run = run_plugin_tool(&source, "{}").await;
    assert_eq!(ok_text(&run.outcome), "[true,true,false,\"tool\"]");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pure_cell_has_an_empty_ceiling() {
    let run =
        eval("[ctx.can(\"env.read\"), ctx.describe()[\"ceiling\"], ctx.describe()[\"phase\"]]")
            .await;
    assert!(
        run.completed_text()
            .contains("\"value\":[false,[],\"eval\"]"),
        "{}",
        run.completed_text()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn terminal_host_outcomes_cannot_be_swallowed_by_try_call() {
    let table = [
        (HostTerminal::Revoked, "denied", "revoked"),
        (HostTerminal::NestedEval, "denied", "nested_eval"),
        (
            HostTerminal::NestedScriptExport,
            "denied",
            "nested_script_export",
        ),
    ];
    for (terminal, status, code) in table {
        let op = native_op(NativeOp::EnvRead);
        let run = eval_with(
            "r = ctx.try_call(ctx.env.read, key = \"k\")\n\"recovered\"",
            &["env.read"],
            vec![(op, OpOutcome::Terminal(terminal.clone()))],
        )
        .await;
        run.assert_failed(status, code);
        assert_eq!(run.calls(), 1, "{terminal:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_is_interrupted_not_a_script_failure() {
    let op = native_op(NativeOp::EnvRead);
    let run = eval_with(
        "ctx.try_call(ctx.env.read, key = \"k\")\n\"recovered\"",
        &["env.read"],
        vec![(op, OpOutcome::Terminal(HostTerminal::Cancelled))],
    )
    .await;
    assert!(
        matches!(run.outcome, ToolOutcome::Interrupted),
        "{:?}",
        run.outcome
    );
    assert_eq!(run.finishes(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn every_host_failure_code_reaches_the_script_intact() {
    let codes = [
        FailureCode::Failed,
        FailureCode::ExitNonZero,
        FailureCode::Unavailable,
        FailureCode::Conflict,
        FailureCode::Busy,
        FailureCode::Cancelled,
        FailureCode::ObservationUnavailable,
        FailureCode::InvocationMismatch,
        FailureCode::Indeterminate,
        FailureCode::Domain("budget_exhausted".into()),
    ];
    for code in codes {
        let op = native_op(NativeOp::EnvRead);
        let reply = failed(op.clone(), code.clone(), "host said no");
        let direct = eval_with(
            "ctx.env.read(key = \"k\")",
            &["env.read"],
            vec![(op.clone(), reply.clone())],
        )
        .await;
        let message = direct.assert_failed("failed", code.as_str());
        assert!(message.contains("host said no"), "{message}");
        let recovered = eval_with(
            "r = ctx.try_call(ctx.env.read, key = \"k\")\nr.error.code",
            &["env.read"],
            vec![(op, reply)],
        )
        .await;
        assert!(
            recovered
                .completed_text()
                .contains(&format!("\"value\":\"{}\"", code.as_str())),
            "{code:?}: {}",
            recovered.completed_text()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unscripted_host_reply_is_an_unavailable_failure_not_a_fake_success() {
    let run = eval_with("ctx.env.read(key = \"k\")", &["env.read"], Vec::new()).await;
    run.assert_failed("failed", "unavailable");
}

// --- plugin tools -----------------------------------------------------------

struct PluginRun {
    host: Arc<RecordingHost>,
    outcome: ToolOutcome,
}

async fn run_plugin_tool(source: &str, arguments: &str) -> PluginRun {
    let (_data, system) = system_with_plugin("probe", source);
    let extensions = system.extensions().expect("plugin converts");
    let tool = named_tool(&extensions[0], "probe__t");
    let host = RecordingHost::new(&extensions, OpSet::EMPTY, []);
    let args = RawJson::parse(arguments).expect("tool arguments are JSON");
    let outcome = tool
        .run(
            ToolCall::new("adversarial", args),
            tool_cx(host.script_cx()),
        )
        .await;
    PluginRun { host, outcome }
}

fn failure_text(outcome: &ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Err(error) => error.to_string(),
        other => panic!("expected a failed tool outcome, got {other:?}"),
    }
}

fn ok_text(outcome: &ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Ok(output) => match &output.parts[0] {
            Part::Text { text } => text.to_string(),
            other => panic!("expected text output, got {other:?}"),
        },
        other => panic!("expected a successful tool outcome, got {other:?}"),
    }
}

fn plugin_with(run: &str, schema: &str, extra: &str) -> String {
    format!(
        "load(\"@dal/v1\", \"dal\")\n{run}\nt = dal.tool(description = \"d\", input = dal.schema({schema}), run = run{extra})\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {{\"t\": t}})\n"
    )
}

const RETURN_ARGS: &str = "def run(ctx, args):\n    return args\n";

#[tokio::test(flavor = "multi_thread")]
async fn tool_arguments_are_validated_before_the_handler_is_entered() {
    let source = plugin_with(
        RETURN_ARGS,
        "n = dal.integer(min = 1, max = 3), s = dal.string(max_len = 4)",
        "",
    );
    let cases = [
        ("{}", "required field is missing"),
        ("{\"n\":1}", "required field is missing"),
        ("{\"n\":1,\"s\":\"ab\",\"x\":1}", "unknown field"),
        ("{\"n\":\"1\",\"s\":\"a\"}", "expected integer"),
        ("{\"n\":1.5,\"s\":\"a\"}", "expected integer"),
        ("{\"n\":null,\"s\":\"a\"}", "expected integer"),
        ("{\"n\":true,\"s\":\"a\"}", "expected integer"),
        ("[]", "expected object"),
        ("{\"n\":0,\"s\":\"a\"}", "below min"),
        ("{\"n\":4,\"s\":\"a\"}", "above max"),
        ("{\"n\":1,\"s\":\"abcde\"}", "above max_len"),
        ("{\"n\":1,\"n\":2,\"s\":\"a\"}", "duplicate key"),
    ];
    for (arguments, needle) in cases {
        let run = run_plugin_tool(&source, arguments).await;
        let message = failure_text(&run.outcome);
        assert!(message.contains(needle), "{arguments}: {message}");
        assert!(
            !run.host
                .records()
                .iter()
                .any(|record| matches!(record, HostRecord::Begin { .. })),
            "{arguments}: invalid arguments must not enter the handler"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_argument_bounds_are_inclusive_on_both_sides() {
    let source = plugin_with(
        RETURN_ARGS,
        "n = dal.integer(min = 1, max = 3), s = dal.string(min_len = 1, max_len = 4)",
        "",
    );
    for arguments in [
        "{\"n\":1,\"s\":\"a\"}",
        "{\"n\":3,\"s\":\"abcd\"}",
        "{\"n\":2,\"s\":\"\u{1f600}\u{1f600}\u{1f600}\u{1f600}\"}",
    ] {
        let run = run_plugin_tool(&source, arguments).await;
        assert!(
            matches!(run.outcome, ToolOutcome::Ok(_)),
            "{arguments}: {:?}",
            run.outcome
        );
    }
    let run = run_plugin_tool(&source, "{\"n\":1,\"s\":\"\"}").await;
    assert!(failure_text(&run.outcome).contains("below min_len"));
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_tool_argument_json_is_a_typed_failure() {
    let source = plugin_with(RETURN_ARGS, "", "");
    for arguments in ["{\"a\":9007199254740992}", "{\"a\":1e999}"] {
        let Ok(args) = RawJson::parse(arguments) else {
            continue;
        };
        let (_data, system) = system_with_plugin("probe", &source);
        let extensions = system.extensions().expect("plugin converts");
        let tool = named_tool(&extensions[0], "probe__t");
        let host = RecordingHost::new(&extensions, OpSet::EMPTY, []);
        let outcome = tool
            .run(
                ToolCall::new("adversarial", args),
                tool_cx(host.script_cx()),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Err(_)),
            "{arguments}: {outcome:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_without_a_script_host_fails_closed() {
    let (_data, system) = system_with_plugin("probe", &plugin_with(RETURN_ARGS, "", ""));
    let extensions = system.extensions().expect("plugin converts");
    let tool = named_tool(&extensions[0], "probe__t");
    let args = RawJson::parse("{}").expect("json");
    let outcome = tool
        .run(
            ToolCall::new("adversarial", args),
            ToolCx::for_test(test_services()),
        )
        .await;
    assert!(
        failure_text(&outcome).contains("does not run plugin handlers"),
        "{outcome:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn handlers_with_the_wrong_shape_fail_each_call_and_still_finish() {
    let cases = [
        "def run(ctx):\n    return None\n",
        "def run(ctx, args, extra):\n    return None\n",
        "def run(*, ctx, args):\n    return None\n",
    ];
    for run_source in cases {
        let run = run_plugin_tool(&plugin_with(run_source, "", ""), "{}").await;
        let message = failure_text(&run.outcome);
        assert_ne!(message, "");
        let finished = run
            .host
            .records()
            .iter()
            .any(|record| matches!(record, HostRecord::Finish { .. }));
        assert!(
            finished,
            "{run_source}: the begun invocation must be finished"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_callable_run_fails_every_call_with_a_typed_error() {
    let source = "load(\"@dal/v1\", \"dal\")\nt = dal.tool(description = \"d\", input = dal.schema(), run = \"not callable\")\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {\"t\": t})\n";
    let run = run_plugin_tool(source, "{}").await;
    let message = failure_text(&run.outcome);
    assert!(
        message.contains("not callable") || message.contains("call"),
        "{message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn handler_results_outside_the_transport_set_fail_the_call() {
    for body in [
        "return lambda: 1",
        "return {1: 2}",
        "return float(\"inf\")",
        "return 1 << 60",
        "return ctx",
        "return (1, lambda: 1)",
    ] {
        let run_source = format!("def run(ctx, args):\n    {body}\n");
        let run = run_plugin_tool(&plugin_with(&run_source, "", ""), "{}").await;
        assert!(
            matches!(run.outcome, ToolOutcome::Err(_)),
            "{body}: {:?}",
            run.outcome
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn output_schema_violations_fail_the_call_and_bounds_are_inclusive() {
    let output = ", output = dal.schema(n = dal.integer(min = 1, max = 3))";
    let make = |value: &str| {
        plugin_with(
            &format!("def run(ctx, args):\n    return {value}\n"),
            "",
            output,
        )
    };
    for value in ["{\"n\": 1}", "{\"n\": 3}"] {
        let run = run_plugin_tool(&make(value), "{}").await;
        assert!(
            matches!(run.outcome, ToolOutcome::Ok(_)),
            "{value}: {:?}",
            run.outcome
        );
    }
    for (value, needle) in [
        ("{\"n\": 0}", "below min"),
        ("{\"n\": 4}", "above max"),
        ("{}", "required field is missing"),
        ("{\"n\": 1, \"extra\": 2}", "unknown field"),
        ("[1]", "expected object"),
        ("None", "expected object"),
    ] {
        let run = run_plugin_tool(&make(value), "{}").await;
        let message = failure_text(&run.outcome);
        assert!(message.contains(needle), "{value}: {message}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dal_err_cannot_mint_a_reserved_host_code() {
    for code in [
        "failed",
        "exit_nonzero",
        "unavailable",
        "conflict",
        "busy",
        "cancelled",
        "observation_unavailable",
        "invocation_mismatch",
        "indeterminate",
    ] {
        let source = plugin_with(
            &format!("def run(ctx, args):\n    return dal.err(\"{code}\", \"spoof\")\n"),
            "",
            "",
        );
        let run = run_plugin_tool(&source, "{}").await;
        let message = failure_text(&run.outcome);
        assert!(
            message.contains("reserved host failure code"),
            "{code}: {message}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dal_output_rejects_views_that_cannot_render() {
    let cases = [
        ("{\"type\": \"text\"}", "text"),
        (
            "{\"type\": \"table\", \"columns\": [1], \"rows\": []}",
            "column labels",
        ),
        (
            "{\"type\": \"table\", \"columns\": [\"a\"], \"rows\": [5]}",
            "must be a list",
        ),
        (
            "{\"type\": \"table\", \"columns\": [\"a\"], \"rows\": [[[1]]]}",
            "scalar",
        ),
        ("{\"type\": \"table\", \"columns\": [\"a\"]}", "rows"),
        ("{\"type\": \"chart\"}", "view node must be"),
        ("5", "view node must be"),
        (
            "{\"copied\": {\"path\": \"a\", \"line\": 1, \"text\": \"t\"}}",
            "view node must be",
        ),
    ];
    for (view, needle) in cases {
        let source = plugin_with(
            &format!("def run(ctx, args):\n    return dal.output(value = 1, view = {view})\n"),
            "",
            "",
        );
        let run = run_plugin_tool(&source, "{}").await;
        let message = failure_text(&run.outcome);
        assert!(message.contains(needle), "{view}: {message}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn output_view_nesting_boundary_is_sixty_four() {
    let make = |depth: usize| {
        plugin_with(
            &format!(
                "def run(ctx, args):\n    v = {{\"type\": \"text\", \"text\": \"t\"}}\n    for _ in range({depth}):\n        v = [v]\n    return dal.output(value = 1, view = v)\n"
            ),
            "",
            "",
        )
    };
    let ok = run_plugin_tool(&make(63), "{}").await;
    assert!(matches!(ok.outcome, ToolOutcome::Ok(_)), "{:?}", ok.outcome);
    let over = run_plugin_tool(&make(66), "{}").await;
    assert!(
        failure_text(&over.outcome).contains("nests too deeply"),
        "{:?}",
        over.outcome
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_handlers_cannot_enter_other_scripted_exports_outside_the_catalog() {
    let source = plugin_with(
        "def run(ctx, args):\n    return ctx.tools[\"probe.ghost\"]()\n",
        "",
        "",
    );
    let run = run_plugin_tool(&source, "{}").await;
    assert!(
        matches!(run.outcome, ToolOutcome::Err(_)),
        "{:?}",
        run.outcome
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_native_facade_attribute_is_reachable_and_nothing_else_is() {
    let probe = eval("dir(ctx)").await;
    let text = probe.completed_text();
    for facade in [
        "tools", "models", "net", "ask", "state", "agents", "jobs", "turn", "env", "mcp", "config",
    ] {
        assert!(
            text.contains(&format!("\"{facade}\"")),
            "ctx lacks {facade}: {text}"
        );
    }
    // `OpId::Native` and export ids are only minted by attribute lookup:
    // an op name from another facade is an attribute error, not a call.
    for code in [
        "ctx.env.fetch",
        "ctx.net.read",
        "ctx.tools.infer",
        "ctx.state.exec",
    ] {
        let run = eval(code).await;
        run.assert_failed("failed", "eval_error");
    }
    assert_eq!(probe.calls(), 0);
}

// --- a host that misbehaves -------------------------------------------------

async fn eval_faulty(fault: Fault, code: &str) -> Run {
    let extension = eval_extension(CELL_LIMITS).expect("eval extension");
    let tool = named_tool(&extension, "eval");
    let host = RecordingHost::faulty(&[extension], fault);
    eval_on(tool, host, &eval_args(code)).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_host_call_is_an_indeterminate_failure() {
    let run = eval_faulty(Fault::Call, "ctx.env.read(key = \"k\")").await;
    let message = run.assert_failed("failed", "indeterminate");
    assert!(message.contains("host call failed"), "{message}");
    assert_eq!((run.begins(), run.finishes()), (1, 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_host_collect_is_not_reported_as_cancellation() {
    for code in [
        "s = ctx.scope(limit = 1)\ns.all()",
        "s = ctx.scope(limit = 1)\ns.settle()",
        "s = ctx.scope(limit = 1)\nt = s.tools.read(path = \"a\")\nt.wait()",
        "s = ctx.scope(limit = 1)\nt = s.tools.read(path = \"a\")\nt.settle()",
    ] {
        let run = eval_faulty(Fault::Collect, code).await;
        assert!(
            !matches!(run.outcome, ToolOutcome::Interrupted),
            "{code}: a host panic must not look like a user cancellation"
        );
        run.assert_failed("failed", "indeterminate");
    }
}

// --- host results the codec must bound --------------------------------------

async fn read_with_reply(json: &str, code: &str) -> Run {
    let op = native_op(NativeOp::EnvRead);
    eval_with(code, &[], vec![(op.clone(), completed_json(op, json))]).await
}

#[tokio::test(flavor = "multi_thread")]
async fn host_result_payload_boundary_is_one_mebibyte() {
    const MAX_DATA: usize = 1 << 20;
    let quoted = |total: usize| format!("\"{}\"", "r".repeat(total - 2));
    for total in [MAX_DATA - 1, MAX_DATA] {
        let run = read_with_reply(&quoted(total), "len(ctx.env.read(key = \"k\"))").await;
        assert_eq!(run.envelope().status, "completed", "{total} bytes");
    }
    let run = read_with_reply(&quoted(MAX_DATA + 1), "ctx.env.read(key = \"k\")").await;
    let message = run.assert_failed("failed", "api_error");
    assert!(message.contains("payload exceeds"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_host_results_are_api_errors_not_values() {
    let cases = [
        ("{\"a\":1,\"a\":2}", "duplicate key"),
        ("9007199254740992", "53-bit"),
        ("-9007199254740992", "53-bit"),
    ];
    for (json, needle) in cases {
        let run = read_with_reply(json, "ctx.env.read(key = \"k\")").await;
        assert_eq!(run.envelope().status, "failed", "{json}");
        let message = run.assert_failed("failed", "api_error");
        assert!(message.contains(needle), "{json}: {message}");
    }
    let deep = format!("{}{}", "[".repeat(66), "]".repeat(66));
    let run = read_with_reply(&deep, "ctx.env.read(key = \"k\")").await;
    let message = run.assert_failed("failed", "api_error");
    assert!(message.contains("nests deeper"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn host_result_depth_and_integer_bounds_are_inclusive() {
    // The top-level value is level 0, so 65 nested empty arrays reach level 64.
    let at_cap = format!("{}{}", "[".repeat(65), "]".repeat(65));
    let below_cap = format!("{}{}", "[".repeat(64), "]".repeat(64));
    for json in [
        at_cap.as_str(),
        below_cap.as_str(),
        "9007199254740991",
        "-9007199254740991",
    ] {
        let run = read_with_reply(json, "None").await;
        assert_eq!(run.envelope().status, "completed");
        let run = read_with_reply(json, "ctx.env.read(key = \"k\")\nNone").await;
        assert_eq!(run.envelope().status, "completed", "{json}");
    }
}

// --- scoped inference over the recording host -------------------------------

const VALID_REQUEST: &str = r#"{"purpose": "turn", "model": {"kind": "api", "family": "openai_chat", "model": "gpt-6"}, "system": "s", "tools": [], "context": [], "params": {"thinking": "off", "effort": None, "temperature": None, "max_output_tokens": None}, "cache_key": None}"#;

#[tokio::test(flavor = "multi_thread")]
async fn scoped_infer_reaches_the_host_as_a_model_submit_and_seals_on_settle() {
    let run = eval(&format!(
        "s = ctx.scope(limit = 2)\ns.infer({VALID_REQUEST})\nr = s.settle()\nlen(r)"
    ))
    .await;
    assert!(
        run.completed_text().contains("\"value\":0"),
        "{}",
        run.completed_text()
    );
    let records = run.records();
    let submitted = records.iter().any(|record| {
        matches!(record, HostRecord::Submit { request, .. }
            if request.op == OpId::Native(NativeOp::ModelsInfer))
    });
    assert!(submitted, "{records:?}");
    let sealed = records.iter().any(|record| {
        matches!(record, HostRecord::Collect { which, .. }
            if matches!(which, dal_agent::ext::script::Collect::Seal(_)))
    });
    assert!(
        sealed,
        "settle must seal and collect the scope: {records:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scoped_infer_with_a_malformed_request_never_reaches_the_host() {
    for request in ["{}", "5", "{\"purpose\": \"turn\", \"bogus\": 1}"] {
        let run = eval(&format!("s = ctx.scope(limit = 2)\ns.infer({request})")).await;
        run.assert_failed("failed", "api_error");
        assert!(
            !run.records()
                .iter()
                .any(|record| matches!(record, HostRecord::Submit { .. })),
            "{request}"
        );
    }
}
