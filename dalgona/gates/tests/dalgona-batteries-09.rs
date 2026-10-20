// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! /review runs the reviewer on the working diff; clean trees report none.
#![expect(
    clippy::disallowed_methods,
    reason = "gate builds real git fixtures for the review tool"
)]
use dalgona_gates::support;

use std::{io, path::Path, process::Command, time::Duration};

use dal_agent::{Delivery, Host, SessionRef, Subscription};
use dal_core::{
    Answer, ClientId, Command as DalCommand, Output, PageReq, Question, Reply, UpdateKind,
    Workspace,
};

const WAIT: Duration = Duration::from_secs(30);

const ONLY_REVIEW: &str = "disabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"procs\", \"quality\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]";
const REVIEWER_FINDING: &str = r#"{"verdict":"findings","findings":[{"path":"notes.txt","line":null,"severity":"major","title":"Broken behavior","detail":"A failure."}],"summary":"reviewed"}"#;

fn git(workspace: &Path, args: &[&str]) -> support::TestResult<()> {
    let output = support::run_command(
        Command::new("git")
            .args(args)
            .current_dir(workspace)
            .env("GIT_OPTIONAL_LOCKS", "0"),
    )?;
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// A workspace git repo whose working tree holds exactly `staged_diff`.
fn git_workspace(root: &Path, committed: &str, worked: &str) -> support::TestResult<PathBuf> {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace)?;
    git(&workspace, &["init"])?;
    std::fs::write(workspace.join("notes.txt"), committed)?;
    git(
        &workspace,
        &[
            "-c",
            "user.email=gate@test",
            "-c",
            "user.name=gate",
            "add",
            "notes.txt",
        ],
    )?;
    git(
        &workspace,
        &[
            "-c",
            "user.email=gate@test",
            "-c",
            "user.name=gate",
            "commit",
            "-m",
            "gate fixture",
        ],
    )?;
    std::fs::write(workspace.join("notes.txt"), worked)?;
    Ok(workspace)
}

use std::path::PathBuf;

/// A live host over the real product with a scripted provider.
async fn review_host(root: &Path, steps: &[String]) -> support::TestResult<Host> {
    let script = root.join("script.jsonl");
    std::fs::write(&script, steps.join("\n"))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n{ONLY_REVIEW}\n[providers.scripted]\nfixture = {:?}\n",
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

/// The newest tool result in the session view.
fn newest_tool_result(agent: &dal_agent::Agent, tool: &str) -> support::TestResult<String> {
    let view = agent.view(PageReq::default())?;
    view.entries
        .items
        .into_iter()
        .rev()
        .find_map(|entry| match entry.kind {
            dal_core::EntryKind::ToolResult { name, parts, .. } if name.as_ref() == tool => {
                Some(format!("{parts:?}"))
            }
            _ => None,
        })
        .ok_or_else(|| io::Error::other(format!("no {tool} result in view")).into())
}

/// Runs /review in a fresh session and returns the command reply once the
/// turn the command submitted has ended.
async fn run_review(
    host: &Host,
    workspace: Workspace,
) -> support::TestResult<(Reply, dal_agent::Agent)> {
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("review-gate"),
        )
        .await?;
    let mut subscription: Subscription = agent.subscribe(None)?;
    let reply = agent
        .submit(DalCommand::Run {
            name: "review".into(),
            args: "".into(),
            expected: None,
        })
        .await?;
    // The command reply does not settle the turn it submitted; the report
    // lands in the view only when the turn ends. The review battery needs a
    // run+infer grant on first use; approve it so the reviewer round runs.
    while let Some(delivery) = tokio::time::timeout(WAIT, subscription.next()).await? {
        if let Delivery::Update(update) = delivery
            && handle_update(&agent, &update).await?
        {
            break;
        }
    }
    Ok((reply, agent))
}

/// Answers the review battery's one-time grant ask; returns true on turn end.
async fn handle_update(
    agent: &dal_agent::Agent,
    update: &dal_core::Update,
) -> support::TestResult<bool> {
    match &update.kind {
        UpdateKind::RequestOpened(request) if matches!(&request.question, Question::Grant { ext, .. } if ext.as_ref() == "review") =>
        {
            agent.answer(request.id, Answer::ApproveForSession).await?;
            Ok(false)
        }
        UpdateKind::TurnEnded { .. } => Ok(true),
        _ => Ok(false),
    }
}

#[test]
fn review_command_completes_and_reports() -> support::TestResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        // A working-tree diff reviewed by the scripted reviewer reports the
        // finding by its normalized title.
        let scratch = support::Scratch::new("review-report")?;
        let root = scratch.path().to_path_buf();
        let workspace = git_workspace(&root, "line one\n", "line one\nline two\n")?;
        let host = review_host(
            &root,
            &[
                support::tool_step("review-1", "review", r#"{"restart":true}"#),
                support::text_step(REVIEWER_FINDING),
                support::text_step("done"),
            ],
        )
        .await?;
        let (reply, agent) = run_review(&host, Workspace::new(workspace)?).await?;
        assert_eq!(
            reply,
            Reply::Done(Output::Nothing),
            "/review completes after its reviewer round"
        );
        // The turn the command submitted settled inside submit_wait before
        // the command replied, so the report is already in the view.
        let report = newest_tool_result(&agent, "review")?;
        assert!(
            report.contains("Broken behavior"),
            "the report names the finding: {report}"
        );
        let key = agent.view(PageReq::default())?.session.id;
        host.close(key).await?;
        drop(agent);
        host.shutdown(Duration::from_secs(2)).await;

        // A clean tree reports no changes without calling the reviewer.
        let clean_scratch = support::Scratch::new("review-clean")?;
        let clean_root = clean_scratch.path().to_path_buf();
        let clean_workspace = git_workspace(&clean_root, "line one\n", "line one\n")?;
        let clean_host = review_host(
            &clean_root,
            &[
                support::tool_step("review-1", "review", r#"{"restart":true}"#),
                support::text_step("done"),
            ],
        )
        .await?;
        let (clean_reply, clean_agent) =
            run_review(&clean_host, Workspace::new(clean_workspace)?).await?;
        assert_eq!(clean_reply, Reply::Done(Output::Nothing));
        let clean_report = newest_tool_result(&clean_agent, "review")?;
        assert!(
            clean_report.contains("No changes to review."),
            "a clean tree reports none: {clean_report}"
        );
        let key = clean_agent.view(PageReq::default())?.session.id;
        clean_host.close(key).await?;
        drop(clean_agent);
        clean_host.shutdown(Duration::from_secs(2)).await;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
