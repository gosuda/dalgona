#![expect(
    clippy::expect_used,
    reason = "fixtures fail loudly at their setup boundary"
)]
//! An inner provider's `message_replay` capture never leaves a synthetic
//! private loop: the loop has no single closing response to bind it to.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome};
use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelRecord, PrivateTool,
    ToolOutput,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    AssistantRoute, Caps, ClientId, Command, Config, ConfigProduct, EntryKind, Expect, Family,
    ModelId, ModelInfo, ModelRequest, ModelRoute, Name, Part, Reply, ServiceSet, ThinkingLevel,
    ToolClass, ToolSpec, UpdateKind, Workspace,
};
use dal_provider::{ProviderError, StreamEvent as ProviderEvent};

/// A private tool that always succeeds, so a private round can complete.
struct Probe {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Probe {
    fn new() -> Self {
        let name = Name::parse("probe").expect("tool name");
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: "Succeeds when called.".into(),
            parameters: dal_core::RawJson::parse(r#"{"type":"object","properties":{}}"#)
                .expect("schema json"),
            grammar: None,
        });
        Self { name, spec }
    }
}

impl Tool for Probe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { ToolOutcome::Ok(Box::new(ToolOutput::from_text("probed"))) })
    }
}

const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"premium_requests":null,"cost_usd":null}}"#;

fn capture_event(payload: &str) -> String {
    format!(r#"{{"type":"message_replay","capture":{{"status":"captured","payload":{payload}}}}},"#)
}

/// A closing step: text, no calls, an optional capture, usage, stop.
fn closing_step(text: &str, capture: Option<&str>) -> String {
    let capture = capture.map(capture_event).unwrap_or_default();
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"{text}"}},{{"type":"tool_calls_done","calls":[]}},{capture}{USAGE},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

/// A private-call step: one `probe` call, an optional capture riding the
/// tool-call response (the scripted grammar is permissive here), usage, stop.
fn private_step(capture: Option<&str>) -> String {
    let capture = capture.map(capture_event).unwrap_or_default();
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":"p1","name":"probe"}},{{"type":"tool_calls_done","calls":[{{"id":"p1","name":"probe","args":{{"kind":"parsed","value":{{}}}}}}]}},{capture}{USAGE},{{"type":"stop","reason":"tool_use"}}]}}"#
    )
}

/// The wire spelling of one relayed event, for ordering assertions.
fn label(item: &Result<ProviderEvent, ProviderError>) -> &'static str {
    match item {
        Ok(ProviderEvent::TextDelta { .. }) => "text",
        Ok(ProviderEvent::ReasoningDelta { .. }) => "reasoning",
        Ok(ProviderEvent::ToolCallStarted { .. }) => "tool_call_started",
        Ok(ProviderEvent::ToolArgsDelta { .. }) => "tool_args",
        Ok(ProviderEvent::Replay { .. }) => "replay",
        Ok(ProviderEvent::ToolCallsDone { .. }) => "tool_calls_done",
        Ok(ProviderEvent::MessageReplay { .. }) => "message_replay",
        Ok(ProviderEvent::Usage { .. }) => "usage",
        Ok(ProviderEvent::Stop { .. }) => "stop",
        Ok(_) => "other",
        Err(_) => "error",
    }
}

/// Forwards to the scripted Chat route with a private tool bound, drains
/// the relay's output to record its order, then hands the same items on to
/// the session so the journal sees exactly what the relay emitted.
struct RelayModel {
    relayed: Arc<Mutex<Vec<&'static str>>>,
}

impl dal_agent::ext::ModelHandler for RelayModel {
    fn run<'a>(
        &'a self,
        mut request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            request.model = ModelRoute::Api {
                family: Family::Chat,
                model: "gpt-6-luna".into(),
            };
            let private = [PrivateTool(Arc::new(Probe::new()))];
            let mut relay = cx.forward(request, &private).await?;
            let mut items = Vec::new();
            while let Some(item) = relay.next().await {
                items.push(item);
            }
            *self
                .relayed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                items.iter().map(label).collect();
            Ok(EventStream::new(futures::stream::iter(items), || {}))
        })
    }
}

async fn rig(
    steps: &[String],
    relayed: Arc<Mutex<Vec<&'static str>>>,
) -> (tempfile::TempDir, Host) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, steps.join("\n")).expect("fixture");
    let user = format!(
        "model = \"acme/relay-capture\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
    let extension = ExtensionBuilder::new("synth-capture", "0.1.0", ServiceSet::default())
        .expect("builder")
        .model(ModelRecord {
            id: ModelId::parse("acme/relay-capture").expect("model id"),
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: true,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(RelayModel { relayed }),
            export: None,
        })
        .build()
        .expect("extension");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir,
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    (tmp, host)
}

/// One synthetic turn over `steps`: the relay's emitted event order and the
/// session's recorded assistant entries (route, replay payload).
struct Outcome {
    relayed: Vec<&'static str>,
    entries: Vec<(Option<AssistantRoute>, Option<String>)>,
}

async fn run_turn(steps: &[String]) -> Outcome {
    let relayed = Arc::new(Mutex::new(Vec::new()));
    let (tmp, host) = rig(steps, Arc::clone(&relayed)).await;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(tmp.path().join("w")).expect("workspace"),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: "go".into() }],
        })
        .await
        .expect("prompt is accepted");
    assert!(matches!(reply, Reply::Accepted { .. }), "{reply:?}");
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(15), subscription.next())
            .await
            .expect("the turn ended in time")
            .expect("stream open");
        if let Delivery::Update(update) = &delivery
            && matches!(update.kind, UpdateKind::TurnEnded { .. })
        {
            break;
        }
    }
    let view = agent.view(dal_core::PageReq::default()).expect("view");
    let entries = view
        .entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Assistant { route, replay, .. } => Some((
                *route,
                replay.as_ref().map(|payload| payload.as_str().to_owned()),
            )),
            _ => None,
        })
        .collect();
    host.shutdown(Duration::from_secs(1)).await;
    let relayed = relayed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    Outcome { relayed, entries }
}

/// The relay's closing emission, in grammar order and without a capture.
const CLOSING: [&str; 4] = ["text", "tool_calls_done", "usage", "stop"];

#[tokio::test]
async fn a_tool_free_inner_capture_is_not_relayed_ahead_of_tool_calls_done() {
    let outcome = run_turn(&[closing_step("done", Some(r#"{"records":["inner"]}"#))]).await;
    assert_eq!(
        outcome.relayed, CLOSING,
        "the relay emits tool_calls_done, usage, stop and no inner capture"
    );
    assert_eq!(
        outcome.entries,
        [(Some(AssistantRoute::Synthetic), None)],
        "the synthetic route's entry carries no inner provider capture"
    );
}

#[tokio::test]
async fn a_capture_riding_a_private_round_never_binds_to_the_closing_entry() {
    let outcome = run_turn(&[
        private_step(Some(r#"{"records":["round-one"]}"#)),
        closing_step("done", None),
    ])
    .await;
    assert_eq!(
        outcome.relayed,
        ["text", "tool_calls_done", "usage", "stop"],
        "the earlier round's capture is not relayed"
    );
    assert_eq!(
        outcome.entries,
        [(Some(AssistantRoute::Synthetic), None)],
        "a capture of a non-closing round must not journal on the closing entry"
    );
}

#[tokio::test]
async fn captures_in_every_private_round_leave_the_synthetic_entry_bare() {
    let outcome = run_turn(&[
        private_step(Some(r#"{"records":["round-one"]}"#)),
        closing_step("done", Some(r#"{"records":["round-two"]}"#)),
    ])
    .await;
    assert_eq!(
        outcome.relayed, CLOSING,
        "no round's capture reaches the outer stream"
    );
    assert_eq!(
        outcome.entries,
        [(Some(AssistantRoute::Synthetic), None)],
        "neither round's capture journals on the synthetic entry"
    );
}
