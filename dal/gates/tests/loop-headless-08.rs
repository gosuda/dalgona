#![expect(clippy::expect_used, reason = "SC test")]
//! Headless wake-loop bounds: twenty wakes, then rejection and reset.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, path::PathBuf, sync::Arc};

use dal_agent::{
    Env, SessionRef, Subscription,
    ext::{BoxFuture, CommandCx, CommandHandler, Extension, ExtensionBuilder, Services},
};
use dal_core::{
    Command, CommandName, CommandSpec, Config, ConfigProduct, Expect, Output, Part, Reply,
    ServiceSet, TurnCause, UpdateKind, Workspace,
    ext::{TurnOp, TurnOpReply},
};
use support::{TestDir, scripted_session};

struct WakeCommand;

impl CommandHandler for WakeCommand {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, dal_agent::ServiceError>> {
        let text = args.to_owned();
        let caller = cx.caller().clone();
        let services = Arc::clone(cx.services());
        Box::pin(async move {
            let text = if text.trim().is_empty() {
                "wake".into()
            } else {
                text
            };
            // The handler context supplies the host-minted Caller; tests never mint one.
            let _ = wake_once(&services, &caller, &text).await?;
            Ok(Reply::Done(Output::Nothing))
        })
    }
}

async fn wake_once(
    services: &Arc<dyn Services>,
    caller: &dal_agent::ext::Caller,
    text: &str,
) -> Result<TurnOpReply, dal_agent::ServiceError> {
    services
        .turn(
            caller,
            TurnOp::Wake {
                text: text.into(),
                sources: vec!["gate-wake".into()],
                job_ids: Vec::new(),
            },
        )
        .await
}

fn wake_extension() -> Result<Extension, Box<dyn Error + Send + Sync>> {
    // Services::turn enforces inject scope first (services.rs:596): declare Turn or
    // every wake dies on inject denial before reaching the wake limit.
    Ok(
        ExtensionBuilder::new("gate-wake", "0.1.0", ServiceSet::from_names(["turn"])?)?
            .command(
                CommandSpec {
                    name: CommandName::parse("gate-wake")?,
                    summary: "Gate wake-limit probe.".into(),
                    args_hint: None,
                },
                Arc::new(WakeCommand),
            )
            .build()?,
    )
}

async fn wait_wake_turn(updates: &mut Subscription) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut saw_wake_start = false;
    let mut saw_end = false;
    while let Some(delivery) =
        tokio::time::timeout(std::time::Duration::from_secs(30), updates.next())
            .await
            .expect("wake turn must end")
    {
        let dal_agent::Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::TurnStarted {
                cause: TurnCause::Wake,
                ..
            } => saw_wake_start = true,
            UpdateKind::TurnEnded { .. } if saw_wake_start => {
                saw_end = true;
                break;
            }
            _ => {}
        }
    }
    assert!(
        saw_wake_start && saw_end,
        "wake tool must start a Wake-caused turn"
    );
    Ok(())
}

#[tokio::test]
async fn wake_limit_allows_twenty_then_rejects_and_resets()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/replay/stress-scripted.jsonl");
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let mut product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    product.extensions.push(wake_extension()?);
    let env = Env {
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let mut updates = harness.agent.subscribe(None)?;

    for _ in 0..20 {
        let reply = harness
            .agent
            .submit(Command::Run {
                name: "gate-wake".into(),
                args: "{\"text\":\"wake\"}".into(),
                expected: None,
            })
            .await?;
        assert!(matches!(reply, Reply::Done(_)));
        wait_wake_turn(&mut updates).await?;
    }

    let twenty_first = harness
        .agent
        .submit(Command::Run {
            name: "gate-wake".into(),
            args: "{\"text\":\"wake\"}".into(),
            expected: None,
        })
        .await;
    // Landed code renders the typed DenyReason::WakeLimit denial as
    // "command denied: WakeLimit" (fold Rejection path, command.rs:1402) or
    // "wake refused: 20 turns ..." (Services path, error.rs:408-411).
    assert!(
        twenty_first.is_err_and(|error| {
            let text = error.to_string();
            text.contains("WakeLimit") || text.contains("wake refused")
        }),
        "21st wake must fail with the typed WakeLimit denial"
    );

    let user_prompt = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "reset wake count".into(),
            }],
        })
        .await?;
    assert!(matches!(user_prompt, Reply::Accepted { .. }));

    let after_reset = harness
        .agent
        .submit(Command::Run {
            name: "gate-wake".into(),
            args: "{\"text\":\"wake\"}".into(),
            expected: None,
        })
        .await?;
    assert!(matches!(after_reset, Reply::Done(_)));
    let _ = harness
        .host
        .shutdown(std::time::Duration::from_secs(1))
        .await;
    Ok(())
}
