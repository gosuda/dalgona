//! A hook minted in a subagent session carries the parent session id; at
//! the root the parent is absent.
#![expect(
    clippy::expect_used,
    reason = "test fixtures abort at the failed boundary"
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::HookCx;
use dal_agent::{Env, Host, HostError, Product, SessionRef};
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

/// One host with the parent-recording hook and three scripted turns.
struct Harness {
    host: Host,
    workspace: Workspace,
    seen: Arc<Mutex<Vec<Option<SessionId>>>>,
    _tmp: tempfile::TempDir,
}

/// Starts the host; `agents` is appended to the user configuration.
async fn harness(agents: &str) -> Harness {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, STEP_END.repeat(3)).expect("fixture");
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n{agents}",
        fixture.to_string_lossy().replace('\\', "\\\\")
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
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    Harness {
        host,
        workspace,
        seen,
        _tmp: tmp,
    }
}

impl Harness {
    async fn open_root(&self) -> dal_agent::Agent {
        self.host
            .open(
                SessionRef::New {
                    workspace: self.workspace.clone(),
                    name: None,
                },
                ClientId::new("probe"),
            )
            .await
            .expect("open root")
    }

    async fn open_child(
        &self,
        parent: SessionId,
        call: &str,
    ) -> Result<dal_agent::Agent, HostError> {
        self.host
            .open(
                SessionRef::Child {
                    parent,
                    call: CallId::new(call),
                    workspace: self.workspace.clone(),
                    name: None,
                },
                ClientId::new("probe"),
            )
            .await
    }

    fn seen(&self) -> Vec<Option<SessionId>> {
        self.seen.lock().expect("seen lock").clone()
    }
}

#[tokio::test]
async fn hook_parent_is_root_in_subagents_and_absent_at_root() {
    let rig = harness("").await;
    let root = rig.open_root().await;
    run_prompt(&root).await.expect("root prompt");
    let root_id = root
        .view(dal_core::PageReq::default())
        .expect("view")
        .session
        .id;

    let child = rig
        .open_child(root_id, "parent-probe")
        .await
        .expect("open child");
    run_prompt(&child).await.expect("child prompt");
    let seen = rig.seen();
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

/// The parent a hook is handed is the session that started its session, not
/// the root of the tree: under `agents.max_depth = 2` a grandchild's hook
/// sees the child. At the default depth of 1 that grandchild is refused with
/// the depth limit named.
#[tokio::test]
async fn hook_parent_is_the_immediate_parent_and_depth_one_refuses_a_grandchild() {
    let deep = harness("\n[agents]\nmax_depth = 2\n").await;
    let root = deep.open_root().await;
    let root_id = root.session();
    let child = deep
        .open_child(root_id, "to-child")
        .await
        .expect("depth 1 opens under max_depth 2");
    let child_id = child.session();
    let grandchild = deep
        .open_child(child_id, "to-grandchild")
        .await
        .expect("depth 2 opens under max_depth 2");
    for agent in [&root, &child, &grandchild] {
        run_prompt(agent).await.expect("prompt");
    }
    assert_eq!(
        deep.seen(),
        vec![None, Some(root_id), Some(child_id)],
        "each session's hook sees the session that started it"
    );

    let shallow = harness("").await;
    let root = shallow.open_root().await;
    let child = shallow
        .open_child(root.session(), "to-child")
        .await
        .expect("depth 1 opens under the default limit");
    let Err(HostError::Config { message }) =
        shallow.open_child(child.session(), "to-grandchild").await
    else {
        panic!("a grandchild must be refused at the default depth limit");
    };
    assert_eq!(
        message.as_ref(),
        HostError::max_depth(1).to_string(),
        "the refusal names the depth limit"
    );
}
