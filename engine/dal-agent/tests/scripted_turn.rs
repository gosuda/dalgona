//! A scripted provider drives one real turn through the public API.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use dal_agent::ext::{BoxFuture, ExtensionBuilder, Hook, HookCx, HookError};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::ext::{InputEvent, InputVerdict, SidecarOp};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, Origin, Part, ServiceSet, SidecarName,
    TurnOp, TurnOpReply, Workspace,
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
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, FIXTURE).expect("fixture");
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
        extensions: Vec::new(),
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
