//! A hook minted in a subagent session carries the parent session id; at
//! the root the parent is absent.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::HookCx;
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::{
    CallId, ClientId, Command, Config, ConfigProduct, Expect, Part, SessionId, Workspace,
};

const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// Records the minted parent of every `before_turn` hook invocation.
struct ParentHook {
    seen: Arc<Mutex<Vec<Option<SessionId>>>>,
}

impl dal_agent::ext::Hook<dal_core::ext::BeforeTurn, Option<String>> for ParentHook {
    fn call(
        &self,
        _input: dal_core::ext::BeforeTurn,
        cx: HookCx,
    ) -> dal_agent::ext::BoxFuture<'static, Result<Option<String>, dal_agent::ext::HookError>> {
        let mut seen = self
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        seen.push(cx.parent);
        drop(seen);
        Box::pin(async { Ok(None) })
    }
}

/// Runs one prompt on `agent` to completion or a short timeout.
async fn run_prompt(agent: &dal_agent::Agent) -> Result<(), Box<dyn std::error::Error>> {
    let mut subscription = agent.subscribe(None)?;
    let _ = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "record the parent".into(),
            }],
        })
        .await?;
    for _ in 0..8 {
        let delivery = tokio::time::timeout(Duration::from_secs(3), subscription.next()).await;
        let Ok(Some(delivery)) = delivery else {
            break;
        };
        if let dal_agent::Delivery::Update(update) = &delivery
            && matches!(update.kind, dal_core::UpdateKind::TurnEnded { .. })
        {
            break;
        }
    }
    Ok(())
}

#[tokio::test]
async fn hook_parent_is_root_in_subagents_and_absent_at_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, format!("{STEP_END}{STEP_END}")).expect("fixture");
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.display()
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let extension =
        dal_agent::ext::ExtensionBuilder::new("parentprobe", "0.1.0", dal_core::ServiceSet::EMPTY)
            .expect("builder")
            .on_before_turn(ParentHook {
                seen: Arc::clone(&seen),
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
    let workspace = Workspace::new(workspace_dir.clone()).expect("workspace");

    let root = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open root");
    run_prompt(&root).await.expect("root prompt");
    let root_id = root
        .view(dal_core::PageReq::default())
        .expect("view")
        .session
        .id;

    let child = host
        .open(
            SessionRef::Child {
                parent: root_id,
                call: CallId::new("parent-probe"),
                workspace: workspace.clone(),
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open child");
    run_prompt(&child).await.expect("child prompt");
    let seen = seen.lock().expect("seen lock").clone();
    // `before_turn` mints once per turn at the Opening phase; one prompt on
    // each session is two fires total.
    assert_eq!(seen.len(), 2, "one fire per turn: seen={seen:?}");
    assert!(
        seen[..1].iter().all(Option::is_none),
        "root turn sees no parent: seen={seen:?}"
    );
    assert!(
        seen[1..].iter().all(|parent| *parent == Some(root_id)),
        "subagent turn sees the root as parent: seen={seen:?}"
    );
}
