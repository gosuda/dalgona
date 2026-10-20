//! Scripted model registrations through the host model context.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    Answer, Command, Config, ConfigProduct, Expect, PageReq, Part, Stop, UpdateKind, Workspace,
};

pub mod support;

use support::system_with_plugin;

fn model_source(run: &str) -> String {
    format!(
        r#"load("@dal/v1", "dal")
def run(ctx, request):
{run}

model = dal.model(
    id = "dalgona/fusion",
    caps = {{
        "context_window": 100000,
        "thinking": ["off"],
        "tool_use": True,
        "image_input": False,
    }},
    run = run,
    uses = ["models.forward", "models.infer"],
)
plugin = dal.plugin(name = "fusion", version = "0.1.0", inject = ["infer"], models = {{"fusion": model}})
"#,
        run = run
            .lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

async fn host_for(run: &str) -> (tempfile::TempDir, Host) {
    host_with_usage(run, 1, 1).await
}

/// A host whose scripted provider replays one forwarded answer `replays`
/// times, each reporting `input_tokens` of input against the priced route.
async fn host_with_usage(
    run: &str,
    replays: usize,
    input_tokens: u64,
) -> (tempfile::TempDir, Host) {
    let (data, system) = system_with_plugin("fusion", &model_source(run));
    let extensions = system.extensions().expect("model extension conversion");
    let fixture = data.path().join("script.jsonl");
    let line = format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"forwarded"}},{{"type":"tool_calls_done","calls":[]}},{{"type":"usage","usage":{{"input_tokens":{input_tokens},"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}}},{{"type":"stop","reason":"end_turn"}}]}}"#
    );
    std::fs::write(&fixture, vec![line; replays].join("\n")).expect("provider replay fixture");
    let user = format!(
        "model = \"dalgona/fusion\"\n\n[providers.scripted]\nfixture = {:?}\n\n[prices.gpt-6-luna]\ninput = 1.0\ncached_input = 0.5\noutput = 2.0\nreasoning = 0.0\n",
        fixture.display().to_string()
    );
    let config =
        Config::load(ConfigProduct::Dalgon, data.path(), "", Some(&user)).expect("host config");
    let workspace = data.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let product = Product {
        name: "dal",
        data_root: data.path().to_path_buf(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace,
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env)
        .await
        .expect("host starts");
    (data, host)
}

async fn prompt_with_answer(
    host: &Host,
    data: &tempfile::TempDir,
    answer: Answer,
) -> (Stop, String, Vec<String>) {
    let workspace = Workspace::new(data.path().join("workspace")).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            dal_core::ClientId::new("model-test"),
        )
        .await
        .expect("session opens");
    let mut subscription = agent.subscribe(None).expect("subscription opens");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "input".into(),
            }],
        })
        .await
        .expect("prompt submits");
    let mut seen = Vec::new();
    let stop = loop {
        let delivery = tokio::time::timeout(Duration::from_secs(10), subscription.next())
            .await
            .expect("turn update arrives");
        let Some(delivery) = delivery else {
            panic!("subscription closed before the turn ended");
        };
        let Delivery::Update(update) = delivery else {
            continue;
        };
        seen.push(format!("{:?}", update.kind));
        if let UpdateKind::RequestOpened(request) = &update.kind {
            agent
                .answer(request.id, answer.clone())
                .await
                .expect("grant answer lands");
            continue;
        }
        if let UpdateKind::TurnEnded { stop, .. } = update.kind {
            break stop;
        }
    };
    let view = format!(
        "{:?}",
        agent.view(PageReq::default()).expect("session view")
    );
    (stop, view, seen)
}

async fn prompt(host: &Host, data: &tempfile::TempDir) -> (Stop, String, Vec<String>) {
    prompt_with_answer(host, data, Answer::ApproveForSession).await
}

const FORWARD_REQUEST: &str = r#"return ctx.models.forward(
    purpose = request.purpose,
    model = {"kind": "api", "family": "openai_chat", "model": "gpt-6-luna"},
    system = request.system,
    tools = request.tools,
    context = request.context,
    params = request.params,
    cache_key = request.cache_key,
)"#;

#[tokio::test]
async fn scripted_model_forwards_through_its_invocation_context() {
    let (data, host) = host_for(FORWARD_REQUEST).await;
    let (stop, view, seen) = prompt(&host, &data).await;
    assert_eq!(stop, Stop::EndTurn, "{seen:?} {view}");
    assert!(
        seen.iter().any(|kind| kind.contains("Grant")),
        "forward must request the Infer capability: {seen:?}"
    );
    assert!(view.contains("forwarded"), "{view}");
}

#[tokio::test]
async fn scripted_forward_declined_infer_grant_never_reaches_the_provider() {
    let (data, host) = host_for(FORWARD_REQUEST).await;
    let (stop, view, seen) = prompt_with_answer(&host, &data, Answer::Decline).await;
    assert_eq!(stop, Stop::Failed, "{seen:?} {view}");
    assert!(
        seen.iter().any(|kind| kind.contains("Grant")),
        "forward must open an Infer grant request: {seen:?}"
    );
    assert!(
        !view.contains("forwarded"),
        "denied forward must not return provider output: {view}"
    );
}

/// Two inferences through one concurrency-1 scope with a $0.40 budget, then
/// the per-task verdicts surfaced as the turn text: the first inference runs
/// and is charged, and only the charge decides whether the second may run.
const USD_SCOPE_RUN: &str = r#"scope = ctx.scope(limit = 1, on_error = "settle", usd = 0.40)
req = {
    "purpose": request.purpose,
    "model": {"kind": "api", "family": "openai_chat", "model": "gpt-6-luna"},
    "system": request.system,
    "tools": request.tools,
    "context": request.context,
    "params": request.params,
    "cache_key": request.cache_key,
}
scope.infer(req)
scope.infer(req)
results = scope.settle()
first = results[0]
ev = list(first.value.events)
verdict = "|".join([("ok" if r.ok else r.error.code) for r in results])
head = ev[0]
shown = {k: (verdict if k == "text" else head[k]) for k in head}
return {"model": first.value.model, "events": [shown] + ev[1:]}"#;

/// The USD budget the script asked for reaches the host scope: once the first
/// inference costs more than $0.40 the scope refuses the one queued behind it,
/// and the same script over an inference that costs far less runs both.
#[tokio::test]
async fn scripted_model_usd_budget_stops_the_scope_once_the_first_inference_overspends() {
    // 1,000,000 input tokens at $1.00 per million is $1.00 > $0.40.
    let (data, host) = host_with_usage(USD_SCOPE_RUN, 2, 1_000_000).await;
    let (stop, view, seen) = prompt(&host, &data).await;
    assert_eq!(stop, Stop::EndTurn, "{seen:?} {view}");
    assert!(
        seen.iter().any(|kind| kind.contains("Grant")),
        "scope.infer opened a grant request: {seen:?}"
    );
    assert!(view.contains("ok|cancelled"), "{seen:?} {view}");

    // The control differs only in what the first inference costs.
    let (data, host) = host_with_usage(USD_SCOPE_RUN, 2, 1).await;
    let (stop, view, seen) = prompt(&host, &data).await;
    assert_eq!(stop, Stop::EndTurn, "control: {seen:?} {view}");
    assert!(view.contains("ok|ok"), "control: {seen:?} {view}");
}

#[tokio::test]
async fn scripted_model_second_forward_call_returns_a_named_failure() {
    let run = r#"first = ctx.models.forward(
    purpose = request.purpose,
    model = {"kind": "api", "family": "openai_chat", "model": "gpt-6-luna"},
    system = request.system,
    tools = request.tools,
    context = request.context,
    params = request.params,
    cache_key = request.cache_key,
)
second = ctx.try_call(
    ctx.models.forward,
    purpose = request.purpose,
    model = {"kind": "api", "family": "openai_chat", "model": "gpt-6-luna"},
    system = request.system,
    tools = request.tools,
    context = request.context,
    params = request.params,
    cache_key = request.cache_key,
)
if second.error.message == "forward may be called at most once":
    return first
return None"#;
    let (data, host) = host_for(run).await;
    let (stop, view, seen) = prompt(&host, &data).await;
    assert_eq!(stop, Stop::EndTurn, "{seen:?} {view}");
    assert!(view.contains("forwarded"), "{seen:?} {view}");
}

const FORWARD_THEN: &str = r#"r = ctx.models.forward(
    purpose = request.purpose,
    model = {"kind": "api", "family": "openai_chat", "model": "gpt-6-luna"},
    system = request.system,
    tools = request.tools,
    context = request.context,
    params = request.params,
    cache_key = request.cache_key,
)
ev = list(r.events)
usage = [e for e in ev if e["type"] == "usage"]
"#;

/// A scripted model must hand the host one well-formed inference record:
/// every malformed return fails the turn instead of reaching the stream.
#[tokio::test]
async fn malformed_scripted_model_returns_fail_the_turn() {
    let tails = [
        ("return None", "no record"),
        ("return \"text\"", "not an object"),
        ("return [r]", "list, not a record"),
        (
            "return {\"model\": r.model, \"events\": ev, \"extra\": 1}",
            "unknown field",
        ),
        ("return {\"model\": r.model}", "missing events"),
        ("return {\"events\": ev}", "missing model"),
        (
            "return {\"model\": r.model, \"events\": ev[:-1]}",
            "missing stop",
        ),
        (
            "return {\"model\": r.model, \"events\": [e for e in ev if e[\"type\"] != \"usage\"]}",
            "missing usage",
        ),
        (
            "return {\"model\": r.model, \"events\": ev + [ev[-1]]}",
            "events after stop",
        ),
        (
            "return {\"model\": r.model, \"events\": ev[:-1] + usage + [ev[-1]]}",
            "duplicate usage",
        ),
        (
            "return {\"model\": r.model, \"events\": [{\"type\": \"compaction\", \"outcome\": {}}] + ev}",
            "compaction or unknown event",
        ),
        (
            "return {\"model\": {\"kind\": \"nope\"}, \"events\": ev}",
            "bad route",
        ),
    ];
    for (tail, label) in tails {
        let (data, host) = host_for(&format!("{FORWARD_THEN}{tail}")).await;
        let (stop, view, seen) = prompt(&host, &data).await;
        assert_eq!(stop, Stop::Failed, "{label}: {seen:?} {view}");
    }
}

/// A scripted return that merely happens to be well-formed must still reach
/// the turn: a host that ignores the script return and replays the fixture
/// would end the turn with the fixture text instead of the mutation.
#[tokio::test]
async fn mutated_scripted_model_fields_reach_the_turn() {
    for marker in ["mutated-alpha", "mutated-beta"] {
        // Rebuild the first event from its live fields, swapping only the
        // text: hand-writing the wire shape would test the codec, not the
        // return path.
        let tail = format!(
            "first = ev[0]\nmut = {{k: (\"{marker}\" if k == \"text\" else first[k]) for k in first}}\nreturn {{\"model\": r.model, \"events\": [mut] + ev[1:]}}"
        );
        let (data, host) = host_for(&format!("{FORWARD_THEN}{tail}")).await;
        let (stop, view, seen) = prompt(&host, &data).await;
        assert_eq!(stop, Stop::EndTurn, "{marker}: {seen:?} {view}");
        assert!(view.contains(marker), "{marker}: {seen:?} {view}");
        assert!(
            !view.contains("forwarded"),
            "the fixture text must not leak past the mutation: {view}"
        );
    }
}

/// The control for the table above: the same scaffolding returning the
/// forwarded record untouched ends the turn normally.
#[tokio::test]
async fn unmodified_scripted_model_record_ends_the_turn() {
    for tail in [
        "return r",
        "return {\"model\": r.model, \"events\": ev}",
        "return {\"model\": r.model, \"events\": ev[:-1] + [ev[-1]]}",
    ] {
        let (data, host) = host_for(&format!("{FORWARD_THEN}{tail}")).await;
        let (stop, view, seen) = prompt(&host, &data).await;
        assert_eq!(stop, Stop::EndTurn, "{tail}: {seen:?} {view}");
    }
}

#[tokio::test]
async fn scripted_model_runtime_error_fails_the_turn_not_the_host() {
    let (data, host) = host_for("fail(\"model script exploded\")").await;
    let (stop, view, seen) = prompt(&host, &data).await;
    assert_eq!(stop, Stop::Failed, "{seen:?} {view}");
    let (second, view, seen) = prompt(&host, &data).await;
    assert_eq!(
        second,
        Stop::Failed,
        "the host stays usable: {seen:?} {view}"
    );
}

/// A USD-budgeted scope opens fine; scheduling inference on a route with no
/// price (the fixture prices only `gpt-6-luna`) is refused at submit, so the
/// turn fails. The control differs only by the missing `scope.infer` call and
/// must end normally, so the refusal is attributed to the inference, not to
/// the budget scope.
#[tokio::test(flavor = "multi_thread")]
async fn usd_budget_scope_refuses_inference_on_an_unpriced_route() {
    let open = "scope = ctx.scope(limit = 8, on_error = \"settle\", usd = 0.40)\n";
    let forward = r#"return ctx.models.forward(
    purpose = request.purpose,
    model = {"kind": "api", "family": "openai_chat", "model": "gpt-6-luna"},
    system = request.system,
    tools = request.tools,
    context = request.context,
    params = request.params,
    cache_key = request.cache_key,
)"#;
    let infer = r#"scope.infer({
    "purpose": request.purpose,
    "model": {"kind": "api", "family": "openai_chat", "model": "unpriced-model"},
    "system": request.system,
    "tools": request.tools,
    "context": request.context,
    "params": request.params,
    "cache_key": request.cache_key,
})
"#;
    let (data, host) = host_for(&format!("{open}{forward}")).await;
    let (stop, view, seen) = prompt(&host, &data).await;
    assert_eq!(stop, Stop::EndTurn, "control: {seen:?} {view}");

    let (data, host) = host_for(&format!("{open}{infer}{forward}")).await;
    let (stop, view, seen) = prompt(&host, &data).await;
    assert_eq!(stop, Stop::Failed, "unpriced inference: {seen:?} {view}");
}
