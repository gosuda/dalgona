//! Extension grant, budget, cancellation, rollback, and
//! service-boundary proofs over the public [`Host`] surface.
//!
//! Every test drives a real session: a scripted model ends each turn
//! immediately, native hooks exercise one seam each, and the drain answers
//! broker questions under a per-test policy. The store rollback test uses
//! only the public [`GrantStore`] API.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "fixture setup must fail at its boundary"
)]

use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, ExtensionBuilder, Hook, HookCx, HookError, ModelCx, ModelError, ModelHandler,
    ModelRecord,
};
use dal_agent::{
    Agent, Delivery, DenyReason, Env, GrantKey, GrantStore, Host, Product, ServiceError, SessionRef,
};
use dal_core::ext::BeforeTurn;
use dal_core::{
    Answer, CancelScope, Caps, ClientId, Command, Config, ConfigProduct, Expect, InputEvent,
    InputVerdict, ModelId, ModelRequest, Origin, Part, Question, RawJson, ServiceSet, Stop,
    ThinkingLevel, TurnOp, TurnOpReply, UpdateKind, Workspace,
};
use dal_provider::{ProviderError, StreamEvent};
use tokio::sync::Notify;

const WAIT: Duration = Duration::from_secs(60);

fn failed(message: &str) -> HookError {
    HookError::Failed {
        message: message.to_string().into(),
    }
}

/// A model that ends every turn immediately with minimal usage.
struct Model;

impl ModelHandler for Model {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<dal_agent::ext::EventStream, ModelError>> {
        Box::pin(async move {
            let events: Vec<Result<StreamEvent, ProviderError>> = vec![
                Ok(StreamEvent::TextDelta {
                    text: "done".into(),
                }),
                Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
                Ok(StreamEvent::Usage {
                    usage: dal_core::Usage {
                        input_tokens: 1,
                        cached_input_tokens: 0,
                        output_tokens: 1,
                        reasoning_tokens: None,
                        cache_write_tokens: 0,
                        cost_usd: None,
                        premium_requests: None,
                    },
                }),
                Ok(StreamEvent::Stop {
                    reason: dal_provider::StopReason::EndTurn,
                }),
            ];
            Ok(dal_agent::ext::EventStream::new(
                futures::stream::iter(events),
                || {},
            ))
        })
    }
}

fn model_extension() -> dal_agent::ext::Extension {
    ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::default())
        .expect("model builder")
        .model(ModelRecord {
            id: ModelId::parse("fixture/end").expect("model id"),
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(Model),
            export: None,
        })
        .build()
        .expect("model extension")
}

async fn start_host(
    extensions: Vec<dal_agent::ext::Extension>,
    data: std::path::PathBuf,
    workspace_dir: &std::path::Path,
) -> Host {
    let mut all = extensions;
    all.push(model_extension());
    let config = Config::load(
        ConfigProduct::Dalgon,
        &data,
        "",
        Some("model = \"fixture/end\"\n"),
    )
    .expect("config");
    Host::start(
        Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions: all,
            bundled: Vec::new(),
        },
        config,
        Env {
            vars: BTreeMap::new(),
            cwd: workspace_dir.to_path_buf(),
            sandbox_helper: None,
        },
    )
    .await
    .expect("host")
}

async fn open_session(host: &Host, workspace_dir: &std::path::Path) -> Agent {
    host.open(
        SessionRef::New {
            workspace: Workspace::new(workspace_dir.to_path_buf()).expect("workspace"),
            name: None,
        },
        ClientId::new("controller"),
    )
    .await
    .expect("session")
}

fn prompt(text: &str) -> Command {
    Command::Prompt {
        content: vec![Part::Text { text: text.into() }],
        expect: Expect::Idle,
    }
}

/// How the drain answers broker questions.
#[derive(Clone, Copy, Debug)]
enum Policy {
    /// No questions may open; any question fails the test.
    Silent,
    /// Decline every grant question.
    DeclineGrants,
}

/// Drains `turns` turn ends, answering questions per policy. Returns the
/// observed stop of every turn plus any hook failure notices.
async fn drain(
    agent: &Agent,
    subscription: &mut dal_agent::Subscription,
    turns: usize,
    policy: Policy,
) -> (Vec<Stop>, Vec<String>, usize) {
    let mut stops = Vec::new();
    let mut notices = Vec::new();
    let mut grants = 0;
    tokio::time::timeout(WAIT, async {
        while stops.len() < turns {
            let delivery = subscription.next().await.expect("session remains open");
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => match (&request.question, policy) {
                    (Question::Grant { .. }, Policy::DeclineGrants) => {
                        grants += 1;
                        agent
                            .answer(request.id, Answer::Decline)
                            .await
                            .expect("decline the grant");
                    }
                    (question, _) => {
                        panic!("unexpected request under {policy:?}: {question:?}")
                    }
                },
                UpdateKind::TurnEnded { stop, .. } => stops.push(*stop),
                UpdateKind::Notice(notice) if notice.kind.as_ref().starts_with("hook.") => {
                    notices.push(notice.text.to_string());
                }
                _ => {}
            }
        }
    })
    .await
    .expect("turns finish before their deadlines");
    (stops, notices, grants)
}

/// A hook that calls `fs_read` on the target input and records the outcome.
struct DenyProbe {
    seen: Arc<std::sync::Mutex<Option<String>>>,
    denied_inject: Arc<AtomicUsize>,
}

impl Hook<InputEvent, InputVerdict> for DenyProbe {
    fn call(
        &self,
        event: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let seen = Arc::clone(&self.seen);
        let denied_inject = Arc::clone(&self.denied_inject);
        let target = event
            .content
            .iter()
            .any(|part| matches!(part, Part::Text { text } if text.as_ref() == "target"));
        Box::pin(async move {
            if target {
                let outcome = cx.services.fs_read(&cx.caller, "probe.txt").await;
                if matches!(outcome, Err(ServiceError::Denied(DenyReason::NotInjected))) {
                    denied_inject.fetch_add(1, Ordering::SeqCst);
                }
                *seen.lock().expect("seen") = Some(format!("{outcome:?}"));
            }
            Ok(InputVerdict::Continue)
        })
    }
}

#[tokio::test]
async fn denied_service_without_inject_fails_closed_without_a_grant_question() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data");
    std::fs::create_dir_all(&workspace_dir).expect("workspace");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let denied_inject = Arc::new(AtomicUsize::new(0));
    let extension = ExtensionBuilder::new("probe-deny", "0.1.0", ServiceSet::default())
        .expect("builder")
        .with_origin(Origin::User, None)
        .on_input(DenyProbe {
            seen: Arc::clone(&seen),
            denied_inject: Arc::clone(&denied_inject),
        })
        .build()
        .expect("extension");
    let host = start_host(vec![extension], data, &workspace_dir).await;
    let agent = open_session(&host, &workspace_dir).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    agent.submit(prompt("target")).await.expect("submit");

    let (stops, notices, grants) = drain(&agent, &mut subscription, 1, Policy::Silent).await;

    assert_eq!(stops, vec![Stop::EndTurn], "the turn still ends cleanly");
    assert!(notices.is_empty(), "no hook failure notice: {notices:?}");
    assert_eq!(
        grants, 0,
        "no grant question opens for an uninjected service"
    );
    assert_eq!(
        denied_inject.load(Ordering::SeqCst),
        1,
        "the call fails Denied(NotInjected): {:?}",
        seen.lock().expect("seen")
    );
}

/// A hook that calls `fs_read` with the service injected; the test declines
/// the grant, so the call must fail `Declined` before any backend effect.
struct GrantProbe {
    denied_grant: Arc<AtomicUsize>,
}

impl Hook<InputEvent, InputVerdict> for GrantProbe {
    fn call(
        &self,
        event: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let denied_grant = Arc::clone(&self.denied_grant);
        let target = event
            .content
            .iter()
            .any(|part| matches!(part, Part::Text { text } if text.as_ref() == "target"));
        Box::pin(async move {
            if target {
                let outcome = cx.services.fs_read(&cx.caller, "probe.txt").await;
                if matches!(outcome, Err(ServiceError::Declined)) {
                    denied_grant.fetch_add(1, Ordering::SeqCst);
                } else {
                    return Err(failed(&format!("expected Declined, got {outcome:?}")));
                }
            }
            Ok(InputVerdict::Continue)
        })
    }
}

#[tokio::test]
async fn declined_grant_denies_the_call_and_ends_the_turn() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data");
    std::fs::create_dir_all(&workspace_dir).expect("workspace");
    let denied_grant = Arc::new(AtomicUsize::new(0));
    let extension = ExtensionBuilder::new(
        "probe-grant",
        "0.1.0",
        ServiceSet::from_names(["fs.read"]).expect("services"),
    )
    .expect("builder")
    .with_origin(Origin::User, None)
    .on_input(GrantProbe {
        denied_grant: Arc::clone(&denied_grant),
    })
    .build()
    .expect("extension");
    let host = start_host(vec![extension], data, &workspace_dir).await;
    let agent = open_session(&host, &workspace_dir).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    agent.submit(prompt("target")).await.expect("submit");

    let (stops, notices, grants) = drain(&agent, &mut subscription, 1, Policy::DeclineGrants).await;

    assert_eq!(grants, 1, "exactly one grant question opens");
    assert_eq!(
        denied_grant.load(Ordering::SeqCst),
        1,
        "the declined grant denies the call"
    );
    assert_eq!(stops, vec![Stop::EndTurn], "denial is fail-closed");
    assert!(notices.is_empty(), "no hook failure notice: {notices:?}");
}

fn grant_key(ext: &str, workspace: &std::path::Path) -> GrantKey {
    GrantKey {
        extension: ext.parse().expect("name"),
        origin: Origin::User,
        services: ServiceSet::from_names(["fs.read"]).expect("services"),
        digest: "a".repeat(64).into_boxed_str(),
        workspace: workspace.to_path_buf(),
    }
}

#[tokio::test]
async fn revoke_rolls_back_one_grant_and_keeps_its_sibling() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).expect("data");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let store = GrantStore::new(data.clone());
    let by = ClientId::new("tui");
    let at = jiff::Timestamp::now();
    let alpha = grant_key("alpha", &workspace);
    let beta = grant_key("beta", &workspace);

    assert!(
        store
            .grant(alpha.clone(), by.clone(), at)
            .await
            .expect("grant alpha")
    );
    assert!(
        store
            .grant(beta.clone(), by.clone(), at)
            .await
            .expect("grant beta")
    );
    assert!(
        !store
            .grant(alpha.clone(), by.clone(), at)
            .await
            .expect("duplicate is a no-op")
    );
    assert_eq!(store.list().await.expect("list").len(), 2);
    assert!(
        data.join("grants.toml").exists(),
        "persistent rows land in grants.toml"
    );

    assert_eq!(
        store
            .revoke(&"alpha".parse().expect("name"))
            .await
            .expect("revoke"),
        1
    );

    assert!(
        !store.contains(&alpha).await.expect("alpha gone"),
        "revocation removes the key"
    );
    assert!(
        store.contains(&beta).await.expect("beta stays"),
        "the sibling grant survives"
    );
    assert_eq!(store.list().await.expect("list").len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(data.join("grants.toml"))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "persistent approvals are owner-only");
    }
    let mut redigest = alpha.clone();
    redigest.digest = "b".repeat(64).into_boxed_str();
    assert!(
        !store.contains(&redigest).await.expect("redigest"),
        "changed code identity is a new key"
    );
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    let mut rehome = alpha.clone();
    rehome.workspace = elsewhere;
    assert!(
        !store.contains(&rehome).await.expect("rehome"),
        "changed workspace is a new key"
    );
    assert_eq!(
        store
            .revoke(&"alpha".parse().expect("name"))
            .await
            .expect("revoke"),
        0,
        "revocation is idempotent"
    );

    let empty = GrantKey {
        extension: "alpha".parse().expect("name"),
        origin: Origin::User,
        services: ServiceSet::default(),
        digest: "a".repeat(64).into_boxed_str(),
        workspace: workspace.clone(),
    };
    assert!(
        store.grant(empty, by, at).await.is_err(),
        "an empty service set grants nothing"
    );
}

/// First input hook: holds until the test releases it. Second input hook:
/// counts entries; cancellation must keep it at zero.
struct HoldFirst {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl Hook<InputEvent, InputVerdict> for HoldFirst {
    fn call(
        &self,
        _event: InputEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Ok(InputVerdict::Continue)
        })
    }
}

struct Follower {
    count: Arc<AtomicUsize>,
}

impl Hook<InputEvent, InputVerdict> for Follower {
    fn call(
        &self,
        _event: InputEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(InputVerdict::Continue) })
    }
}

#[tokio::test]
async fn mid_execution_cancel_stops_later_input_hooks() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data");
    std::fs::create_dir_all(&workspace_dir).expect("workspace");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let follower = Arc::new(AtomicUsize::new(0));
    let extension = ExtensionBuilder::new("probe-cancel", "0.1.0", ServiceSet::default())
        .expect("builder")
        .with_origin(Origin::Builtin, None)
        .on_input(HoldFirst {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        })
        .on_input(Follower {
            count: Arc::clone(&follower),
        })
        .build()
        .expect("extension");
    let host = start_host(vec![extension], data, &workspace_dir).await;
    let agent = open_session(&host, &workspace_dir).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    let turn = match agent.submit(prompt("target")).await.expect("submit") {
        dal_core::Reply::Accepted { turn, .. } => turn,
        reply => panic!("prompt must be accepted, got {reply:?}"),
    };
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("first hook runs");
    agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(turn),
        })
        .await
        .expect("cancel");
    release.notify_one();

    let (stops, notices, _) = drain(&agent, &mut subscription, 1, Policy::Silent).await;

    assert_eq!(stops.len(), 1, "the turn ends exactly once");
    assert_ne!(
        stops[0],
        Stop::EndTurn,
        "cancellation ends the turn: {:?}",
        stops[0]
    );
    assert_eq!(
        follower.load(Ordering::SeqCst),
        0,
        "no later hook starts after cancel"
    );
    assert!(
        notices.is_empty(),
        "cancel reports no hook failure: {notices:?}"
    );
}

/// One side of the records boundary: appends one row, then reads back the
/// caller's own rows of the same kind.
struct RecordParty {
    body: &'static str,
    read_count: Arc<AtomicUsize>,
    foreign_seen: Arc<AtomicUsize>,
}

impl Hook<InputEvent, InputVerdict> for RecordParty {
    fn call(
        &self,
        event: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let target = event
            .content
            .iter()
            .any(|part| matches!(part, Part::Text { text } if text.as_ref() == "target"));
        let read_count = Arc::clone(&self.read_count);
        let foreign_seen = Arc::clone(&self.foreign_seen);
        let body = self.body;
        Box::pin(async move {
            if target {
                let parsed = RawJson::parse(body).map_err(|error| failed(&error.to_string()))?;
                cx.services
                    .append_record(&cx.caller, "note", parsed.into())
                    .await
                    .map_err(|error| failed(&error.to_string()))?;
                let rows = cx
                    .services
                    .records(&cx.caller, "note")
                    .await
                    .map_err(|error| failed(&error.to_string()))?;
                read_count.fetch_add(rows.len(), Ordering::SeqCst);
                for row in &rows {
                    if !row.as_str().contains(body) {
                        foreign_seen.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
            Ok(InputVerdict::Continue)
        })
    }
}

#[tokio::test]
async fn cross_extension_records_stay_invisible() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data");
    std::fs::create_dir_all(&workspace_dir).expect("workspace");
    let mut counts = Vec::new();
    let mut foreign = Vec::new();
    let mut extensions = Vec::new();
    for (ext, body) in [("rec-a", r#"{"by":"a"}"#), ("rec-b", r#"{"by":"b"}"#)] {
        let read_count = Arc::new(AtomicUsize::new(0));
        let foreign_seen = Arc::new(AtomicUsize::new(0));
        counts.push(Arc::clone(&read_count));
        foreign.push(Arc::clone(&foreign_seen));
        extensions.push(
            ExtensionBuilder::new(ext, "0.1.0", ServiceSet::default())
                .expect("builder")
                .with_origin(Origin::Builtin, None)
                .on_input(RecordParty {
                    body,
                    read_count,
                    foreign_seen,
                })
                .build()
                .expect("extension"),
        );
    }
    let host = start_host(extensions, data, &workspace_dir).await;
    let agent = open_session(&host, &workspace_dir).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    agent.submit(prompt("target")).await.expect("submit");

    let (stops, notices, _) = drain(&agent, &mut subscription, 1, Policy::Silent).await;

    assert_eq!(stops, vec![Stop::EndTurn]);
    assert!(notices.is_empty(), "{notices:?}");
    for (index, count) in counts.iter().enumerate() {
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "party {index} reads exactly its own row"
        );
    }
    for (index, seen) in foreign.iter().enumerate() {
        assert_eq!(
            seen.load(Ordering::SeqCst),
            0,
            "party {index} sees no foreign row"
        );
    }
}

/// Budget probe: wakes every turn and records when the 20-wake budget
/// refuses the next wake.
struct WakeProbe {
    woken: Arc<AtomicUsize>,
    refused: Arc<std::sync::Mutex<Option<String>>>,
}

impl Hook<BeforeTurn, Option<String>> for WakeProbe {
    fn call(
        &self,
        _event: BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let woken = Arc::clone(&self.woken);
        let refused = Arc::clone(&self.refused);
        Box::pin(async move {
            match cx
                .services
                .turn(
                    &cx.caller,
                    TurnOp::Wake {
                        text: "next".into(),
                        sources: vec!["probe".into()],
                        job_ids: Vec::new(),
                    },
                )
                .await
            {
                Ok(TurnOpReply::Woken) => {
                    woken.fetch_add(1, Ordering::SeqCst);
                }
                other => {
                    *refused.lock().expect("refused") = Some(format!("{other:?}"));
                }
            }
            Ok(None)
        })
    }
}

#[tokio::test]
async fn wake_budget_refuses_past_twenty_without_a_user_prompt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data");
    std::fs::create_dir_all(&workspace_dir).expect("workspace");
    let woken = Arc::new(AtomicUsize::new(0));
    let refused = Arc::new(std::sync::Mutex::new(None));
    let extension = ExtensionBuilder::new(
        "wake-probe",
        "0.1.0",
        ServiceSet::from_names(["turn"]).expect("services"),
    )
    .expect("builder")
    .with_origin(Origin::Builtin, None)
    .on_before_turn(WakeProbe {
        woken: Arc::clone(&woken),
        refused: Arc::clone(&refused),
    })
    .build()
    .expect("extension");
    let host = start_host(vec![extension], data, &workspace_dir).await;
    let agent = open_session(&host, &workspace_dir).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    agent.submit(prompt("seed")).await.expect("submit");

    // One prompt-started turn plus twenty wake-started turns; the wake in
    // the twentieth wake-started turn is refused, so the chain stops.
    let (stops, notices, _) = drain(&agent, &mut subscription, 21, Policy::Silent).await;

    assert!(stops.iter().all(|stop| *stop == Stop::EndTurn), "{stops:?}");
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(woken.load(Ordering::SeqCst), 20, "twenty wakes queue");
    let text = refused
        .lock()
        .expect("refused")
        .clone()
        .expect("one refusal");
    assert!(
        text.contains("WakeLimit"),
        "the refusal is the wake budget denial: {text}"
    );
}

/// The `ask` service carries no grant: a hook question resolves through the
/// broker even though no capability was ever approved.
struct AskProbe {
    answered: Arc<AtomicUsize>,
}

impl Hook<InputEvent, InputVerdict> for AskProbe {
    fn call(
        &self,
        event: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let answered = Arc::clone(&self.answered);
        let target = event
            .content
            .iter()
            .any(|part| matches!(part, Part::Text { text } if text.as_ref() == "target"));
        Box::pin(async move {
            if target {
                let answer = cx
                    .services
                    .ask(
                        &cx.caller,
                        dal_core::Question::Text {
                            prompt: "hook-probe".into(),
                            placeholder: None,
                        },
                    )
                    .await
                    .map_err(|error| failed(&error.to_string()))?;
                if answer
                    == Some(Answer::Value(
                        RawJson::parse("\"yes\"").map_err(|error| failed(&error.to_string()))?,
                    ))
                {
                    answered.fetch_add(1, Ordering::SeqCst);
                } else {
                    return Err(failed(&format!("ask must resolve, got {answer:?}")));
                }
            }
            Ok(InputVerdict::Continue)
        })
    }
}

#[tokio::test]
async fn ask_resolves_without_any_grant() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data");
    std::fs::create_dir_all(&workspace_dir).expect("workspace");
    let answered = Arc::new(AtomicUsize::new(0));
    let extension = ExtensionBuilder::new(
        "ask-probe",
        "0.1.0",
        ServiceSet::from_names(["ask"]).expect("services"),
    )
    .expect("builder")
    .with_origin(Origin::User, None)
    .on_input(AskProbe {
        answered: Arc::clone(&answered),
    })
    .build()
    .expect("extension");
    let host = start_host(vec![extension], data, &workspace_dir).await;
    let agent = open_session(&host, &workspace_dir).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    agent.submit(prompt("target")).await.expect("submit");

    tokio::time::timeout(WAIT, async {
        let mut ends = 0;
        while ends < 1 {
            let delivery = subscription.next().await.expect("session remains open");
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => match &request.question {
                    dal_core::Question::Text { prompt, .. } if prompt.as_ref() == "hook-probe" => {
                        agent
                            .answer(
                                request.id,
                                Answer::Value(RawJson::parse("\"yes\"").expect("answer")),
                            )
                            .await
                            .expect("answer the ask");
                    }
                    dal_core::Question::Grant { .. } => {
                        panic!("ask must not open a grant question")
                    }
                    other => panic!("unexpected request: {other:?}"),
                },
                UpdateKind::TurnEnded { stop, .. } => {
                    assert_eq!(*stop, Stop::EndTurn, "ask never breaks the turn");
                    ends += 1;
                }
                UpdateKind::Notice(notice) if notice.kind.as_ref().starts_with("hook.") => {
                    panic!("hook failed: {}", notice.text)
                }
                _ => {}
            }
        }
    })
    .await
    .expect("ask resolves before its deadline");
    assert_eq!(answered.load(Ordering::SeqCst), 1, "the ask answer arrives");
}
