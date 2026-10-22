//! A scripted provider drives one real turn through the public API.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, Extension, ExtensionBuilder, Hook, HookCx, HookError, ModelCx, ModelError,
    ModelHandler, ModelRecord,
};
use dal_agent::{Agent, Env, Host, Product, SessionRef, Subscription};
use dal_core::ext::{InputEvent, InputVerdict, OpSet, SidecarOp};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, Expect, ModelId, ModelRequest, ModelRoute,
    Origin, Part, ServiceSet, SidecarName, ThinkingLevel, TurnOp, TurnOpReply, Usage, Workspace,
};

const FIXTURE: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"Hel\"},{\"type\":\"text_delta\",\"text\":\"lo\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

struct NoteHook;

impl dal_agent::ext::Hook<dal_core::ext::BeforeTurn, Option<String>> for NoteHook {
    fn call(
        &self,
        _input: dal_core::ext::BeforeTurn,
        _cx: dal_agent::ext::HookCx,
    ) -> dal_agent::ext::BoxFuture<'static, Result<Option<String>, dal_agent::ext::HookError>> {
        Box::pin(async { Ok(Some(String::from("hook-note"))) })
    }
}

/// Calls two real session-owned services from both opening hook phases.
/// The sidecar write must complete before the idle probe and both must return
/// before the scripted provider receives its first request.
struct OpeningRoundTripHook {
    calls: Arc<AtomicUsize>,
    sidecar: SidecarName,
}

async fn opening_roundtrip(
    services: Arc<dyn dal_agent::ext::Services>,
    caller: dal_agent::ext::Caller,
    sidecar: SidecarName,
    calls: Arc<AtomicUsize>,
) -> Result<(), HookError> {
    services
        .sidecar(
            &caller,
            SidecarOp::Write {
                name: sidecar,
                bytes: b"opening".to_vec(),
            },
        )
        .await
        .map_err(|error| HookError::Failed {
            message: error.to_string().into(),
        })?;
    let idle = services
        .turn(&caller, TurnOp::IsIdle)
        .await
        .map_err(|error| HookError::Failed {
            message: error.to_string().into(),
        })?;
    if !matches!(idle, TurnOpReply::Idle(false)) {
        return Err(HookError::Failed {
            message: "opening turn reported idle".into(),
        });
    }
    calls.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

impl Hook<InputEvent, InputVerdict> for OpeningRoundTripHook {
    fn call(
        &self,
        _input: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let services = Arc::clone(&cx.services);
        let caller = cx.caller;
        let sidecar = self.sidecar.clone();
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            opening_roundtrip(services, caller, sidecar, calls).await?;
            Ok(InputVerdict::Continue)
        })
    }
}

impl Hook<dal_core::ext::BeforeTurn, Option<String>> for OpeningRoundTripHook {
    fn call(
        &self,
        _input: dal_core::ext::BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let services = Arc::clone(&cx.services);
        let caller = cx.caller;
        let sidecar = self.sidecar.clone();
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            opening_roundtrip(services, caller, sidecar, calls).await?;
            Ok(None)
        })
    }
}

/// The `before_turn` text sits in its own reminder right after the user entry.
fn assert_hook_text_is_its_own_reminder(view: &dal_core::View) {
    let items = &view.entries.items;
    let user = items.first().expect("the prompt journals a user entry");
    let hook = items.get(1).expect("the hook text journals a reminder");
    assert!(
        matches!(&user.kind, dal_core::EntryKind::User { parts } if parts.len() == 1),
        "the hook text is not joined to the user entry: {user:?}"
    );
    assert!(
        matches!(
            &hook.kind,
            dal_core::EntryKind::Reminder { source, text }
                if source.as_ref() == dal_core::BEFORE_TURN_SOURCE && text.as_ref() == "hook-note"
        ),
        "the hook text follows as a reminder: {hook:?}"
    );
}

#[tokio::test]
async fn scripted_prompt_runs_a_turn() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, FIXTURE).expect("fixture");
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
    let extension =
        dal_agent::ext::ExtensionBuilder::new("wiretest", "0.1.0", dal_core::ServiceSet::default())
            .expect("builder")
            .on_before_turn(NoteHook)
            .build()
            .expect("extension");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
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
            content: vec![Part::Text { text: "hi".into() }],
        })
        .await
        .expect("submit");
    let dal_core::Reply::Accepted { turn, .. } = reply else {
        panic!("prompt not accepted: {reply:?}");
    };
    let mut saw_end = false;
    let mut seen: Vec<String> = Vec::new();
    let mut last = String::from("no deliveries");
    for _ in 0..20 {
        let delivery = tokio::time::timeout(Duration::from_secs(3), subscription.next()).await;
        let Ok(delivery) = delivery else {
            let page = dal_core::PageReq::default();
            let view = agent.view(page).expect("view");
            let cancel = agent
                .submit(Command::Cancel {
                    scope: dal_core::CancelScope::Turn(turn),
                })
                .await;
            last = format!(
                "seen={} turn={:?} seq={} entries={} cancel={cancel:?}",
                seen.len(),
                view.turn,
                view.seq,
                view.entries.items.len(),
            );
            break;
        };
        let delivery = delivery.expect("stream open");
        if let dal_agent::Delivery::Update(update) = &delivery {
            seen.push(format!("{:?}", update.kind));
            if matches!(update.kind, dal_core::UpdateKind::TurnEnded { .. }) {
                saw_end = true;
                break;
            }
        }
    }
    assert!(
        saw_end,
        "turn runs to TurnEnded on the scripted fixture ({last})"
    );
    let view = agent.view(dal_core::PageReq::default()).expect("view");
    assert_hook_text_is_its_own_reminder(&view);
}

#[tokio::test]
async fn opening_hooks_can_round_trip_through_the_real_session_actor() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, FIXTURE).expect("fixture");
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
    let calls = Arc::new(AtomicUsize::new(0));
    let sidecar = SidecarName::parse("opening.txt").expect("sidecar name");
    let services = ServiceSet::from_names(["sidecar", "turn"]).expect("service set");
    let extension = ExtensionBuilder::new("opening", "0.1.0", services)
        .expect("builder")
        .with_origin(Origin::Builtin, None)
        .on_input(OpeningRoundTripHook {
            calls: Arc::clone(&calls),
            sidecar: sidecar.clone(),
        })
        .on_before_turn(OpeningRoundTripHook {
            calls: Arc::clone(&calls),
            sidecar,
        })
        .build()
        .expect("extension");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace_dir).expect("workspace"),
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
            content: vec![Part::Text { text: "hi".into() }],
        })
        .await
        .expect("submit");
    assert!(matches!(reply, dal_core::Reply::Accepted { .. }));
    // This whole test takes about 3 s on a loaded machine, so the bound only
    // has to outlast that; the count below pins that both hooks completed.
    let stop = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let delivery = subscription.next().await.expect("stream open");
            if let dal_agent::Delivery::Update(update) = delivery
                && let dal_core::UpdateKind::TurnEnded { stop, .. } = update.kind
            {
                break stop;
            }
        }
    })
    .await
    .expect("the turn ends after both opening hooks");
    assert_eq!(
        stop,
        dal_core::Stop::EndTurn,
        "the turn ends normally, not on a hook failure"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "both input and before_turn hooks completed their actor round trips"
    );
}
#[tokio::test]
async fn login_stores_api_key_at_mode_0600() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).expect("data dir");
    let config = Config::load(ConfigProduct::Dalgon, &data, "", None).expect("config");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: data.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let (key, pasted) = tokio::sync::oneshot::channel();
    key.send("sk-test-key".to_owned()).expect("open channel");
    let (io, _progress) = dal_agent::login::LoginIo::channel(
        Some(pasted),
        tokio_util::sync::CancellationToken::new(),
    );
    host.login("openai", dal_agent::login::Method::ApiKey, io)
        .await
        .expect("login");
    let path = data.join("auth.json");
    let text = std::fs::read_to_string(&path).expect("auth.json");
    assert!(text.contains("sk-test-key"), "key persists in the store");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "store is owner-only");
    }
}

/// A host whose only provider is the scripted fixture and whose default model
/// is the family-qualified `openai-responses/gpt-6`, which no catalog lists.
async fn qualified_model_host() -> (Host, Workspace, tempfile::TempDir) {
    scripted_host(FIXTURE, Vec::new()).await
}

/// A host whose scripted provider serves `script` for the default model
/// `openai-responses/gpt-6`, with `extensions` registered.
async fn scripted_host(
    script: &str,
    extensions: Vec<Extension>,
) -> (Host, Workspace, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, script).expect("fixture");
    let user = format!(
        "model = \"openai-responses/gpt-6\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    (host, workspace, tmp)
}

#[tokio::test]
async fn corrected_grants_allow_reopening_a_session_after_failed_startup() {
    let (host, workspace, tmp) = qualified_model_host().await;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("create session");
    let id = agent.session();
    let mut subscription = agent.subscribe(None).expect("subscribe");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: "hi".into() }],
        })
        .await
        .expect("submit");
    let stop = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let delivery = subscription.next().await.expect("stream open");
            if let dal_agent::Delivery::Update(update) = delivery
                && let dal_core::UpdateKind::TurnEnded { stop, .. } = update.kind
            {
                break stop;
            }
        }
    })
    .await
    .expect("seed session completes a turn");
    assert_eq!(stop, dal_core::Stop::EndTurn);
    let saved = agent
        .view(dal_core::PageReq::default())
        .expect("saved view");
    host.close(id).await.expect("close initial session");
    drop(agent);
    let resume = SessionRef::Resume {
        key: id.to_string().into(),
        workspace,
    };
    let grants = tmp.path().join("data/grants.toml");
    let malformed = "not a grant block\n";
    tokio::fs::write(&grants, malformed)
        .await
        .expect("malformed grants fixture");
    let rejected = host.open(resume.clone(), ClientId::new("probe")).await;
    assert!(
        matches!(rejected, Err(dal_agent::HostError::Config { .. })),
        "malformed grants must fail closed: {:?}",
        rejected.as_ref().err()
    );
    assert_eq!(
        tokio::fs::read_to_string(&grants)
            .await
            .expect("read rejected grants"),
        malformed
    );
    tokio::fs::write(&grants, "").await.expect("correct grants");
    let agent = host
        .open(resume, ClientId::new("probe"))
        .await
        .expect("failed startup must release the session journal");
    assert_eq!(agent.session(), id);
    let recovered = agent
        .view(dal_core::PageReq::default())
        .expect("replayed view");
    assert_eq!(recovered.entries.items, saved.entries.items);
    host.close(id).await.expect("close recovered session");
    let _ = host.shutdown(Duration::from_secs(1)).await;
}

/// A family-qualified model id the catalog never lists still reaches the
/// scripted provider: the default route keeps its family but not its prefix.
#[tokio::test]
async fn scripted_turn_ends_on_a_family_qualified_unlisted_model() {
    let (host, workspace, _tmp) = qualified_model_host().await;
    let agent = host
        .open(SessionRef::Ephemeral { workspace }, ClientId::new("probe"))
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: "hi".into() }],
        })
        .await
        .expect("submit");
    let stop = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let delivery = subscription.next().await.expect("stream open");
            if let dal_agent::Delivery::Update(update) = delivery
                && let dal_core::UpdateKind::TurnEnded { stop, .. } = update.kind
            {
                break stop;
            }
        }
    })
    .await
    .expect("turn ends");
    assert_eq!(stop, dal_core::Stop::EndTurn);
    let view = agent.view(dal_core::PageReq::default()).expect("view");
    let replied = view
        .entries
        .items
        .iter()
        .find_map(|entry| match &entry.kind {
            dal_core::EntryKind::Assistant {
                api,
                model,
                content,
                stop,
                ..
            } => {
                let text: String = content
                    .iter()
                    .filter_map(|block| match block {
                        dal_core::Block::Text { text, .. } => Some(text.as_ref()),
                        _ => None,
                    })
                    .collect();
                Some((*api, model.to_string(), text, stop.clone()))
            }
            _ => None,
        });
    assert_eq!(
        replied,
        Some((
            dal_core::Family::Responses,
            "gpt-6".to_owned(),
            "Hello".to_owned(),
            dal_core::AssistantStop::Done,
        )),
        "the journaled reply keeps the route's family and drops its prefix"
    );
}

/// The router relay resolves a route by the same rule as a session turn: a
/// route that names an unlisted model by family reaches its provider.
#[tokio::test]
async fn relay_opens_a_family_route_for_an_unlisted_model() {
    let (host, _workspace, _tmp) = qualified_model_host().await;
    let route = dal_core::ModelRoute::Api {
        family: dal_core::Family::Responses,
        model: "gpt-6".into(),
        provider: None,
    };
    let request = dal_core::ModelRequest {
        purpose: dal_core::Purpose::Turn,
        model: route.clone(),
        system: "".into(),
        tools: std::sync::Arc::from(Vec::new()),
        context: std::sync::Arc::from(vec![dal_core::ContextItem::User {
            parts: vec![Part::Text { text: "hi".into() }],
        }]),
        params: dal_core::RequestParams::default(),
        cache_key: None,
    };
    let mut stream = host
        .relay(ClientId::new("router"), route, request)
        .await
        .expect("relay resolves the unlisted model");
    let mut text = String::new();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("stream item in time")
    {
        if let dal_provider::StreamEvent::TextDelta { text: delta } = item.expect("event") {
            text.push_str(&delta);
        }
    }
    assert_eq!(text, "Hello");
}

/// A capture payload spelled so that any re-encoding shows: member order and
/// interior whitespace must reach the journal and the next request unchanged.
const CAPTURED: &str = r#"{"zeta":1, "alpha" : [1,2]}"#;

/// The `Hello` response up to the capture slot, which follows `tool_calls_done`.
const CAPTURE_HEAD: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"Hel"},{"type":"text_delta","text":"lo"},{"type":"tool_calls_done","calls":[]},"#;

/// The `Hello` response from `usage` to the stop.
const CAPTURE_TAIL: &str = r#"{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;

/// One scripted `Hello` response whose `message_replay` event carries `capture`.
fn capture_script(capture: &str) -> String {
    format!("{CAPTURE_HEAD}{{\"type\":\"message_replay\",\"capture\":{capture}}},{CAPTURE_TAIL}\n")
}

/// A registered model that records every request it serves and answers `ok`.
struct RecordingModel {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl ModelHandler for RecordingModel {
    fn uses(&self) -> OpSet {
        OpSet::parse(["models.infer"]).expect("model uses")
    }

    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<dal_provider::EventStream, ModelError>> {
        self.requests.lock().expect("requests lock").push(request);
        let events = vec![
            dal_provider::StreamEvent::TextDelta { text: "ok".into() },
            dal_provider::StreamEvent::ToolCallsDone { calls: Vec::new() },
            dal_provider::StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    output_tokens: 5,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    cost_usd: None,
                    premium_requests: None,
                },
            },
            dal_provider::StreamEvent::Stop {
                reason: dal_provider::StopReason::EndTurn,
            },
        ];
        Box::pin(async {
            Ok(dal_provider::EventStream::new(
                futures::stream::iter(events.into_iter().map(Ok::<_, dal_provider::ProviderError>)),
                || {},
            ))
        })
    }
}

/// An extension registering `dalgona/capture`, which records its requests.
fn recording_extension(requests: &Arc<Mutex<Vec<ModelRequest>>>) -> Extension {
    ExtensionBuilder::new("capture", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .with_origin(Origin::Builtin, None)
        .model(ModelRecord {
            id: ModelId::parse("dalgona/capture").expect("model id"),
            caps: Caps {
                context_window: Some(4096),
                thinking: Box::new([ThinkingLevel::Off, ThinkingLevel::Low]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(RecordingModel {
                requests: Arc::clone(requests),
            }),
            export: None,
        })
        .build()
        .expect("extension")
}

/// Submits one prompt and returns how its turn ended with the notices it raised.
async fn prompt_until_end(
    agent: &Agent,
    subscription: &mut Subscription,
    text: &str,
) -> (dal_core::Stop, Vec<dal_core::Notice>) {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await
        .expect("submit");
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut notices = Vec::new();
        loop {
            let delivery = subscription.next().await.expect("stream open");
            let dal_agent::Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                dal_core::UpdateKind::Notice(notice) => notices.push(notice.clone()),
                dal_core::UpdateKind::TurnEnded { stop, .. } => break (*stop, notices),
                _ => {}
            }
        }
    })
    .await
    .expect("turn ends")
}

/// The first assistant entry of the view: family, model, content, and the
/// entry-level replay text.
fn first_assistant(
    view: &dal_core::View,
) -> (dal_core::Family, &str, &[dal_core::Block], Option<&str>) {
    view.entries
        .items
        .iter()
        .find_map(|entry| match &entry.kind {
            dal_core::EntryKind::Assistant {
                api,
                model,
                content,
                replay,
                ..
            } => Some((
                *api,
                model.as_ref(),
                content.as_slice(),
                replay.as_ref().map(dal_core::RawJson::as_str),
            )),
            _ => None,
        })
        .expect("the turn journals an assistant entry")
}

/// The record lines of one closed session's journal, found under the store
/// layout `<data_root>/sessions/<workspace key>/<session>/journal.jsonl`.
fn journal_lines(data_root: &std::path::Path, session: dal_core::SessionId) -> Vec<String> {
    let name = session.to_string();
    let journal = std::fs::read_dir(data_root.join("sessions"))
        .expect("sessions dir")
        .map(|workspace| {
            workspace
                .expect("workspace dir")
                .path()
                .join(&name)
                .join("journal.jsonl")
        })
        .find(|candidate| candidate.is_file())
        .expect("the session journal is on disk");
    std::fs::read_to_string(journal)
        .expect("journal text")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A scripted `message_replay` capture lands byte for byte on the journaled
/// assistant entry and rides the next model request as the replay of that
/// entry's context item, with the family and model that produced it.
#[tokio::test]
async fn scripted_message_replay_reaches_the_journal_and_the_next_request() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let capture = format!(r#"{{"status":"captured","payload":{CAPTURED}}}"#);
    let (host, workspace, tmp) = scripted_host(
        &capture_script(&capture),
        vec![recording_extension(&requests)],
    )
    .await;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");

    let (stop, _) = prompt_until_end(&agent, &mut subscription, "hi").await;
    assert_eq!(stop, dal_core::Stop::EndTurn);
    let view = agent.view(dal_core::PageReq::default()).expect("view");
    let (family, model, content, replay) = first_assistant(&view);
    assert_eq!(
        (family, model),
        (dal_core::Family::Responses, "gpt-6"),
        "the entry names the scripted route that produced it"
    );
    assert!(
        matches!(content, [dal_core::Block::Text { text, replay: None }] if text.as_ref() == "Hello"),
        "the capture rides the entry, not a block: {content:?}"
    );
    assert_eq!(replay, Some(CAPTURED), "the live entry holds the capture");

    // The next turn runs on a registered model, which sees the exact request
    // a provider would.
    agent
        .submit(Command::SetModel {
            model: ModelRoute::Synthetic {
                id: "dalgona/capture".into(),
            },
            save: dal_core::Save::SessionOnly,
        })
        .await
        .expect("set model");
    let (stop, _) = prompt_until_end(&agent, &mut subscription, "again").await;
    assert_eq!(stop, dal_core::Stop::EndTurn);
    {
        let seen = requests.lock().expect("requests lock");
        let [request] = seen.as_slice() else {
            panic!("the capture model serves exactly the second prompt: {seen:?}");
        };
        let carried = request
            .context
            .iter()
            .find_map(|item| match item {
                dal_core::ContextItem::Assistant {
                    source,
                    parts,
                    replay,
                } => Some((source, parts, replay)),
                _ => None,
            })
            .expect("the request replays the first response");
        assert_eq!(carried.0.family, family);
        assert_eq!(carried.0.model.as_ref(), model);
        assert_eq!(
            carried.2.as_ref().map(dal_core::RawJson::as_str),
            Some(CAPTURED),
            "the next request carries the capture unmodified"
        );
        assert!(
            matches!(carried.1.as_slice(), [dal_core::AssistantPart::Text { text, replay: None }] if text.as_ref() == "Hello"),
            "the parts stay plain text: {:?}",
            carried.1
        );
    }

    let session = agent.session();
    host.close(session).await.expect("close");
    let lines = journal_lines(&tmp.path().join("data"), session);
    let assistant = lines
        .iter()
        .find(|line| line.contains(r#""type":"assistant""#))
        .expect("the first response journals an assistant record");
    assert!(
        assistant.contains(&format!(r#""replay":{CAPTURED}"#)),
        "the journal line spells the capture verbatim: {assistant}"
    );
}

/// A refused capture journals the response without a payload and fails the
/// turn with the typed reason.
#[tokio::test]
async fn scripted_refused_message_replay_fails_the_turn_and_keeps_the_content() {
    let (host, workspace, tmp) = scripted_host(
        &capture_script(r#"{"status":"refused","reason":"no_records"}"#),
        Vec::new(),
    )
    .await;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");

    let (stop, notices) = prompt_until_end(&agent, &mut subscription, "hi").await;
    assert_eq!(stop, dal_core::Stop::Failed);
    let failure = notices
        .iter()
        .find(|notice| notice.kind.as_ref() == "turn.failed")
        .expect("the refusal raises turn.failed");
    assert!(
        failure.text.contains("no_records"),
        "the notice names the typed reason: {failure:?}"
    );
    let view = agent.view(dal_core::PageReq::default()).expect("view");
    let (_, _, content, replay) = first_assistant(&view);
    assert!(
        matches!(content, [dal_core::Block::Text { text, replay: None }] if text.as_ref() == "Hello"),
        "the response content stays journaled: {content:?}"
    );
    assert_eq!(replay, None, "a refused capture journals no payload");

    let session = agent.session();
    host.close(session).await.expect("close");
    let lines = journal_lines(&tmp.path().join("data"), session);
    let assistant = lines
        .iter()
        .find(|line| line.contains(r#""type":"assistant""#))
        .expect("the response journals an assistant record");
    assert!(
        assistant.contains("Hello") && !assistant.contains(r#""replay":{"#),
        "the assistant line keeps the text and carries no entry replay: {assistant}"
    );
    let turn_end = lines
        .iter()
        .find(|line| line.contains(r#""type":"turn_end""#))
        .expect("the failed turn journals its end");
    assert!(
        turn_end.contains("no_records"),
        "the turn end names the typed reason: {turn_end}"
    );
}

/// A scripted thinking-signature `replay` event flows through the live turn
/// into the journaled reasoning block byte for byte. The session projection
/// passes the payload through untouched and the fold attaches it to the open
/// reasoning block; the assertions pin the exact signature bytes on the live
/// view and on the journal line, so a drop at either hop fails loudly.
#[tokio::test]
async fn scripted_thinking_signature_reaches_the_reasoning_block_and_journal() {
    const ITEM: &str =
        r#"{"type":"thinking","thinking":"deep thought","signature":"EqQBCgIYAhIM-synth-01"}"#;
    let script = concat!(
        r#"{"kind":"events","events":["#,
        r#"{"type":"reasoning_delta","text":"deep thought"},"#,
        r#"{"type":"replay","payload":{"family":"anthropic","model":"claude-test-1","item":{"type":"thinking","thinking":"deep thought","signature":"EqQBCgIYAhIM-synth-01"}}},"#,
        r#"{"type":"tool_calls_done","calls":[]},"#,
        r#"{"type":"usage","usage":{"input_tokens":25,"cached_input_tokens":0,"output_tokens":12,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},"#,
        r#"{"type":"stop","reason":"end_turn"}]}"#,
        "\n",
    );
    let (host, workspace, tmp) = scripted_host(script, Vec::new()).await;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");

    let (stop, _) = prompt_until_end(&agent, &mut subscription, "hi").await;
    assert_eq!(stop, dal_core::Stop::EndTurn);
    let view = agent.view(dal_core::PageReq::default()).expect("view");
    let (_, _, content, replay) = first_assistant(&view);
    assert_eq!(replay, None, "no entry-level capture rides this turn");
    assert!(
        matches!(&content, [dal_core::Block::Reasoning { text, replay }]
            if text.as_ref() == "deep thought" && replay.as_str() == ITEM),
        "the reasoning block keeps the exact signature item: {content:?}"
    );

    let session = agent.session();
    host.close(session).await.expect("close");
    let lines = journal_lines(&tmp.path().join("data"), session);
    let assistant = lines
        .iter()
        .find(|line| line.contains(r#""type":"assistant""#))
        .expect("the thinking response journals an assistant record");
    assert!(
        assistant.contains(ITEM),
        "the journal line spells the signature item verbatim: {assistant}"
    );
}
