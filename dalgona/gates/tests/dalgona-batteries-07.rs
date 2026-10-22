// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Plan mode is real session state; todo writes land in the session journal.
use dalgona_gates::support;

use std::{io, path::Path, time::Duration};

use dal_agent::{Agent, Host, SessionRef, Subscription};
use dal_core::{ClientId, Command, Expect, Output, PageReq, Part, Reply, Workspace};

const WAIT: Duration = Duration::from_secs(30);

fn done_output(reply: Reply) -> support::TestResult<Output> {
    match reply {
        Reply::Done(output) => Ok(output),
        other => Err(io::Error::other(format!("unexpected command reply: {other:?}")).into()),
    }
}

fn output_text(output: &Output) -> String {
    format!("{output:?}")
}

/// A live host over the real product with a scripted provider, work battery on.
async fn work_host(root: &Path, steps: &[String]) -> support::TestResult<Host> {
    let script = root.join("script.jsonl");
    std::fs::write(&script, steps.join("\n"))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\ndisabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\"]\n[providers.scripted]\nfixture = {:?}\n",
        script.display().to_string()
    );
    let factory = dalgona::product();
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        root,
        factory.defaults,
        Some(&user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: root.into(),
        config: &config,
    })?;
    let env = dal_agent::Env {
        vars: std::collections::BTreeMap::from([("OPENAI_API_KEY".into(), "sk-test".into())]),
        cwd: root.into(),
        sandbox_helper: None,
    };
    Ok(Host::start(product, config, env).await?)
}

async fn prompt(
    agent: &Agent,
    subscription: &mut Subscription,
    text: &str,
) -> support::TestResult<()> {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    while let Some(delivery) = tokio::time::timeout(WAIT, subscription.next()).await? {
        if let dal_agent::Delivery::Update(update) = delivery
            && let dal_core::UpdateKind::TurnEnded { .. } = &update.kind
        {
            return Ok(());
        }
    }
    Err("subscription closed before the turn ended".into())
}

async fn run_command(agent: &Agent, name: &str, args: &str) -> support::TestResult<Output> {
    done_output(
        agent
            .submit(Command::Run {
                name: name.into(),
                args: args.into(),
                expected: None,
            })
            .await?,
    )
}

#[tokio::test]
async fn plan_and_todos_commands_use_a_real_session_journal() -> support::TestResult<()> {
    let scratch = support::Scratch::new("work-session-commands")?;
    let root = scratch.path().to_path_buf();
    std::fs::create_dir_all(root.join("workspace"))?;
    let workspace = Workspace::new(root.join("workspace"))?;

    // Plan mode is in-memory session state: toggling with no args flips it.
    let host = work_host(&root, &[support::text_step("done")]).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: Some("work-gate".into()),
            },
            ClientId::new("dalgona-work-gate"),
        )
        .await?;
    let toggled_on = run_command(&agent, "plan", "").await?;
    assert!(
        output_text(&toggled_on).contains("Plan mode is on."),
        "first toggle enables plan mode: {toggled_on:?}"
    );
    let toggled_off = run_command(&agent, "plan", "").await?;
    assert!(
        output_text(&toggled_off).contains("Plan mode is off."),
        "second toggle disables plan mode: {toggled_off:?}"
    );
    let explicit_on = run_command(&agent, "plan", "on").await?;
    assert!(output_text(&explicit_on).contains("Plan mode is on."));
    let explicit_off = run_command(&agent, "plan", "off").await?;
    assert!(output_text(&explicit_off).contains("Plan mode is off."));
    let usage = run_command(&agent, "plan", "someday").await?;
    assert!(output_text(&usage).contains("usage: /plan [on|off]"));
    let empty = run_command(&agent, "todos", "").await?;
    assert_eq!(empty, Output::Markdown("No todo list.".into()));
    let key = agent.view(PageReq::default())?.session.id;
    host.close(key).await?;
    drop(agent);
    host.shutdown(Duration::from_secs(2)).await;

    // A todo write lands in the session journal and replays after resume.
    let write = support::tool_step(
        "todo-1",
        "todo",
        r#"{"action":"write","todos":[{"subject":"Gate packing list","state":"in_progress"}]}"#,
    );
    let host = work_host(&root, &[write, support::text_step("done")]).await?;
    let (agent, mut subscription) = {
        let agent = host
            .open(
                SessionRef::New {
                    workspace: workspace.clone(),
                    name: Some("todo-replay".into()),
                },
                ClientId::new("dalgona-work-gate"),
            )
            .await?;
        let subscription = agent.subscribe(None)?;
        (agent, subscription)
    };
    prompt(&agent, &mut subscription, "track the packing list").await?;
    let shown = run_command(&agent, "todos", "").await?;
    assert!(
        output_text(&shown).contains("Gate packing list"),
        "the written list reads back in-session: {shown:?}"
    );
    let key = agent.view(PageReq::default())?.session.id;
    host.close(key).await?;
    drop(subscription);
    drop(agent);
    host.shutdown(Duration::from_secs(2)).await;

    let host = work_host(&root, &[support::text_step("done")]).await?;
    let agent = host
        .open(
            SessionRef::Resume {
                workspace,
                key: "todo-replay".into(),
            },
            ClientId::new("dalgona-work-gate"),
        )
        .await?;
    let replayed = run_command(&agent, "todos", "").await?;
    assert!(
        output_text(&replayed).contains("Gate packing list"),
        "resume replays the journaled list: {replayed:?}"
    );
    let key = agent.view(PageReq::default())?.session.id;
    host.close(key).await?;
    drop(agent);
    host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}
