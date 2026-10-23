//! Repro for the candidate resume defect: prompting a `RESUMED` session whose
//! previous turn died in its `OPENING` phase ends the new turn Failed.
//!
//! Flow: session with input hook held -> cancel during opening ->
//! `TurnEnded` Cancelled -> close -> reopen -> prompt -> assert fresh `EndTurn`.

#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
#![expect(clippy::panic, reason = "test helpers fail loudly at their boundary")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use dal_agent::ext::{BoxFuture, ExtensionBuilder, Hook, HookCx, HookError};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::ext::{InputEvent, InputVerdict};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, Part, ServiceSet, TurnId, Workspace,
};

const WAIT: Duration = Duration::from_secs(30);

/// One scripted provider request: a complete text stream ending the turn.
const REQUEST: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"Hel\"},{\"type\":\"text_delta\",\"text\":\"lo\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

struct LifecycleHost {
    host: Host,
    workspace: Workspace,
    _tmp: tempfile::TempDir,
}

async fn scripted_host(
    requests: usize,
    extensions: Vec<dal_agent::ext::Extension>,
) -> LifecycleHost {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, REQUEST.repeat(requests)).expect("fixture");
    let config_text = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    LifecycleHost {
        host,
        workspace,
        _tmp: tmp,
    }
}

/// Holds one opening open until the test cancels; the first hold raises
/// `entered` so the test only acts while the hook is in flight.
struct HeldHook {
    armed: Arc<AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    gate: Arc<tokio::sync::Notify>,
}

impl Hook<InputEvent, InputVerdict> for HeldHook {
    fn call(
        &self,
        _input: InputEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let armed = Arc::clone(&self.armed);
        let entered = Arc::clone(&self.entered);
        let gate = Arc::clone(&self.gate);
        let hold = armed.swap(false, Ordering::SeqCst);
        // Gate-only like the reported flow: the held future never observes
        // the cancel itself; the dispatch drops it on the cancel arm.
        Box::pin(async move {
            if hold {
                entered.notify_one();
                gate.notified().await;
            }
            Ok(InputVerdict::Continue)
        })
    }
}

fn prompt(text: &str) -> Command {
    Command::Prompt {
        expect: Expect::Idle,
        content: vec![Part::Text { text: text.into() }],
    }
}

fn assert_accepted(reply: &dal_core::Reply, what: &str) -> TurnId {
    match reply {
        dal_core::Reply::Accepted { turn, .. } => *turn,
        other => panic!("{what} was not accepted: {other:?}"),
    }
}

struct Drain {
    started: bool,
    stop: Option<dal_core::Stop>,
    notices: Vec<String>,
}

async fn drain_until_ended(subscription: &mut dal_agent::Subscription, turn: TurnId) -> Drain {
    let mut drain = Drain {
        started: false,
        stop: None,
        notices: Vec::new(),
    };
    let outcome = tokio::time::timeout(WAIT, async {
        loop {
            let delivery = subscription.next().await.expect("stream open");
            let dal_agent::Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                dal_core::UpdateKind::TurnStarted { turn: seen, .. } if *seen == turn => {
                    drain.started = true;
                }
                dal_core::UpdateKind::TurnEnded { turn: seen, stop } if *seen == turn => {
                    drain.stop = Some(*stop);
                    break;
                }
                dal_core::UpdateKind::Notice(notice) if notice.turn == Some(turn) => {
                    drain
                        .notices
                        .push(format!("{}: {}", notice.kind, notice.text));
                }
                _ => {}
            }
        }
    })
    .await;
    outcome.expect("turn ends in time");
    drain
}

#[tokio::test]
async fn resume_after_opening_cancel_prompts_fresh() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let extension = ExtensionBuilder::new("held", "0.1.0", ServiceSet::default())
        .expect("builder")
        .on_input(HeldHook {
            armed: Arc::new(AtomicBool::new(true)),
            entered: Arc::clone(&entered),
            gate: Arc::new(tokio::sync::Notify::new()),
        })
        .build()
        .expect("extension");
    let live = scripted_host(1, vec![extension]).await;
    let agent = live
        .host
        .open(
            SessionRef::New {
                workspace: live.workspace.clone(),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let id = agent.session();
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let turn = assert_accepted(
        &agent.submit(prompt("cancel-me")).await.expect("submit"),
        "first prompt",
    );
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("the input hook is held");
    let cancel = agent
        .submit(Command::Cancel {
            scope: dal_core::CancelScope::Turn(turn),
        })
        .await;
    assert!(
        matches!(cancel, Ok(dal_core::Reply::Done(_))),
        "opening cancel resolves done, got {cancel:?}"
    );
    let end = drain_until_ended(&mut subscription, turn).await;
    assert_eq!(
        end.stop,
        Some(dal_core::Stop::Cancelled),
        "opening cancel ends the turn cancelled"
    );

    // Close while the cancelled opening turn is still the last turn, then
    // reopen and prompt: the new turn must end fresh.
    let saved = agent
        .view(dal_core::PageReq::default())
        .expect("saved view");
    live.host.close(id).await.expect("close");
    drop(agent);
    drop(subscription);
    let agent = live
        .host
        .open(
            SessionRef::Resume {
                key: id.to_string().into(),
                workspace: live.workspace,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("resume");
    let recovered = agent
        .view(dal_core::PageReq::default())
        .expect("replayed view");
    assert_eq!(
        recovered.entries.items, saved.entries.items,
        "cancelled-opening history survives a durable reopen"
    );
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let turn = assert_accepted(
        &agent.submit(prompt("after-cancel")).await.expect("submit"),
        "post-resume prompt",
    );
    let end = drain_until_ended(&mut subscription, turn).await;
    assert_eq!(
        end.stop,
        Some(dal_core::Stop::EndTurn),
        "a turn that never began must not poison the next prompt after resume; notices: {:?}",
        end.notices
    );
}
