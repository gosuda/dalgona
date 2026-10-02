//! A scripted provider drives one real turn through the public API.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::{ClientId, Command, Config, ConfigProduct, Expect, Part, Workspace};

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
    let dump = format!("{view:?}");
    assert!(
        dump.contains("hook-note"),
        "before_turn text joins the journaled entry ({dump})"
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
    host.login("openai", "sk-test-key").await.expect("login");
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
