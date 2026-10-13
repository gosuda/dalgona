//! SDK scripted path returns assistant text and exits.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, path::PathBuf, time::Duration};

use dal_agent::{Env, Product, SessionRef};
use dal_core::{
    Command, Config, ConfigProduct, Expect, PageReq, Part, Reply, Stop, StreamChannel, UpdateKind,
    Workspace,
};
use support::{TestDir, scripted_session};

#[tokio::test]
async fn sdk_scripted_path_returns_assistant_text_and_exits()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/replay/sdk-scripted.jsonl");
    let user = format!(
        "model = \"openai-responses/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, data.path(), "", Some(&user))?;
    let product = Product {
        name: "dal",
        data_root: data.path().to_path_buf(),
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let env = Env {
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let view = harness
        .agent
        .view(PageReq::default())
        .expect("ephemeral SDK session is readable");
    assert_eq!(view.session.workspace.as_path(), workspace.path());
    let mut subscription = harness.agent.subscribe(None)?;
    let reply = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Reply with exactly one scripted greeting.".into(),
            }],
        })
        .await?;
    assert!(matches!(reply, Reply::Accepted { .. }));

    let mut assistant_text = Vec::new();
    let stop = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let Some(delivery) = subscription.next().await else {
                return Err(std::io::Error::other(
                    "SDK session subscription closed before turn end",
                ));
            };
            let dal_agent::Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::Delta {
                    channel: StreamChannel::Text,
                    text,
                    ..
                } => assistant_text.push(text.to_string()),
                UpdateKind::TurnEnded { stop, .. } => break Ok(*stop),
                _ => {}
            }
        }
    })
    .await??;
    assert_eq!(stop, Stop::EndTurn);
    assert_eq!(assistant_text.len(), 1);
    assert_eq!(
        assistant_text.first().unwrap().as_str(),
        "hello from scripted SDK"
    );

    let shutdown = tokio::time::timeout(
        Duration::from_secs(5),
        harness.host.shutdown(Duration::from_secs(1)),
    )
    .await?;
    assert_eq!(shutdown.sessions_closed, 1);
    Ok(())
}
