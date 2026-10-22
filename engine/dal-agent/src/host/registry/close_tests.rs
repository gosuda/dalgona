//! A close whose caller gives up still runs the session's whole teardown:
//! cancellation, driver abort, actor shutdown, and the removal publication.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use dal_core::ext::TurnEnd;
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, Part, SessionId, Stop, UpdateKind, Workspace,
};
use tokio::sync::Notify;

use crate::agent::{Agent, Delivery, Subscription};
use crate::ext::{BoxFuture, ExtensionBuilder, HookCx, HookError, ObserveHook};
use crate::host::{Env, Host, HostSubscription, HostUpdate, Product, SessionRef};

const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
const GRACE: Duration = Duration::from_millis(200);
const WAIT: Duration = Duration::from_secs(10);

struct HeldTurnEnd {
    entered: Arc<Notify>,
}

impl ObserveHook<TurnEnd> for HeldTurnEnd {
    fn call(&self, _input: TurnEnd, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let entered = Arc::clone(&self.entered);
        Box::pin(async move {
            entered.notify_one();
            std::future::pending().await
        })
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    host: Host,
    agent: Agent,
    session: SessionId,
    subscription: Subscription,
}

async fn session_with_unsettled_turn() -> Rig {
    let entered = Arc::new(Notify::new());
    let extension = ExtensionBuilder::new("held", "0.1.0", dal_core::ServiceSet::EMPTY)
        .expect("builder")
        .on_turn_end(HeldTurnEnd {
            entered: Arc::clone(&entered),
        })
        .build()
        .expect("extension");
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, STEP_END).expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (
                OsString::from("HOME"),
                OsString::from(tmp.path().join("home")),
            ),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(tmp.path().join("cache")),
            ),
        ]),
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
            ClientId::new("close-test"),
        )
        .await
        .expect("session");
    let mut subscription = agent.subscribe(None).expect("subscription");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "hold the settlement".into(),
            }],
        })
        .await
        .expect("prompt accepted");
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            if let Delivery::Update(update) = delivery
                && let UpdateKind::TurnEnded { stop, .. } = update.kind
            {
                assert_eq!(stop, Stop::EndTurn);
                return;
            }
        }
    })
    .await
    .expect("turn ends");
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("turn-end hook entered");
    let session = agent.inner.session;
    Rig {
        _tmp: tmp,
        host,
        agent,
        session,
        subscription,
    }
}

async fn closes(subscription: &mut Subscription) {
    tokio::time::timeout(WAIT, async { while subscription.next().await.is_some() {} })
        .await
        .expect("the actor shut down and closed its subscribers");
}

async fn removed(updates: &mut HostSubscription, session: SessionId) {
    tokio::time::timeout(WAIT, async {
        while let Some(update) = updates.next().await {
            if matches!(update, HostUpdate::SessionRemoved { session: gone } if gone == session) {
                return;
            }
        }
    })
    .await
    .expect("the removal was published");
}

#[tokio::test]
async fn a_close_dropped_at_the_settlement_wait_still_tears_the_session_down() {
    let mut rig = session_with_unsettled_turn().await;
    let mut updates = rig.host.subscribe();
    let dropped = tokio::time::timeout(GRACE, rig.host.close(rig.session)).await;
    assert!(
        dropped.is_err(),
        "the held turn-end hook keeps the settlement past the close grace"
    );
    closes(&mut rig.subscription).await;
    removed(&mut updates, rig.session).await;
    drop(rig.agent);
}

#[tokio::test]
async fn shutdown_past_its_grace_counts_only_sessions_it_tore_down() {
    let mut rig = session_with_unsettled_turn().await;
    let report = rig.host.clone().shutdown_after_quiet_wait(GRACE).await;
    assert_eq!(report.sessions_closed, 1, "the session left the table");
    assert_eq!(
        report.tasks_remaining, 0,
        "no teardown tail outlives shutdown"
    );
    closes(&mut rig.subscription).await;
    drop(rig.agent);
}
