// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#![expect(
    clippy::disallowed_methods,
    reason = "this boundary test intentionally drives the real git executable"
)]

//! The review status cap is enforced after real git output crosses the boundary.

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    Answer, Block, ClientId, Command as SessionCommand, Config, ConfigProduct, EntryKind, Expect,
    JournalPart, PageReq, Part, Record, Reply, UpdateKind, Workspace,
};
use dal_store::Store;
use dalgona_batteries::review::{ReviewConfig, review};

const STATUS_LIMIT: usize = 262_144;
const WAIT: Duration = Duration::from_secs(30);
const PARENT_FOLLOW_UP_TEXT: &str = "parent turn completed";
const STATUS_TOO_LARGE_TEXT: &str =
    "workspace status exceeds 262144 bytes; commit or stash unrelated changes";
const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;

type TestResult = Result<(), Box<dyn StdError>>;

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Result<Self, Box<dyn StdError>> {
        let path = std::env::temp_dir().join(format!(
            "dalgona-review-status-{}",
            dal_core::SessionId::new_v7()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn run_git(workspace: &Path, args: &[&str]) -> Result<Vec<u8>, Box<dyn StdError>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(output.stdout)
}

fn create_oversized_status_workspace(scratch: &Scratch) -> Result<PathBuf, Box<dyn StdError>> {
    let workspace = scratch.path.join("workspace");
    fs::create_dir_all(&workspace)?;
    run_git(&workspace, &["init", "--quiet"])?;
    run_git(
        &workspace,
        &["config", "user.email", "review-test@example.invalid"],
    )?;
    run_git(&workspace, &["config", "user.name", "review status test"])?;
    run_git(
        &workspace,
        &["commit", "--allow-empty", "--quiet", "-m", "baseline"],
    )?;

    for index in 0..1_200 {
        let filename = format!("status-overflow-{index:04}-{}", "x".repeat(220));
        fs::write(workspace.join(filename), b"")?;
    }

    let status = run_git(&workspace, &["status", "--short"])?;
    assert!(
        status.len() > STATUS_LIMIT,
        "fixture status must exceed the review cap: {} bytes",
        status.len()
    );
    let diff = run_git(&workspace, &["diff", "HEAD"])?;
    assert!(
        diff.is_empty(),
        "fixture must overflow status, not diff (diff was {} bytes)",
        diff.len()
    );
    Ok(workspace)
}

fn review_call_step() -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":"review-call","name":"review"}},{{"type":"tool_calls_done","calls":[{{"id":"review-call","name":"review","args":{{"kind":"parsed","value":{{}}}}}}]}},{USAGE},{{"type":"stop","reason":"tool_use"}}]}}"#
    )
}

fn parent_follow_up_step() -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"{PARENT_FOLLOW_UP_TEXT}"}},{{"type":"tool_calls_done","calls":[]}},{USAGE},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

async fn start_review_session(
    scratch: &Scratch,
    workspace: &Path,
) -> Result<(Host, Agent, Subscription), Box<dyn StdError>> {
    let data = scratch.path.join("data");
    fs::create_dir_all(&data)?;
    let fixture = data.join("script.jsonl");
    fs::write(
        &fixture,
        format!("{}\n{}\n", review_call_step(), parent_follow_up_step()),
    )?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(&user))?;
    let extension = review(ReviewConfig::default())?;
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.to_owned(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace.to_owned())?,
                name: None,
            },
            ClientId::new("review-status-overflow"),
        )
        .await?;
    let subscription = agent.subscribe(None)?;
    Ok((host, agent, subscription))
}

async fn wait_for_turn_end(agent: &Agent, subscription: &mut Subscription) -> TestResult {
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => {
                    agent.answer(request.id, Answer::ApproveForSession).await?;
                }
                UpdateKind::TurnEnded { .. } => return Ok(()),
                _ => {}
            }
        }
        Err("subscription closed before the parent turn ended".into())
    })
    .await?
}

fn review_error_text(view: &dal_core::View) -> Option<&str> {
    view.entries.items.iter().find_map(|entry| {
        let EntryKind::ToolResult {
            name,
            error: true,
            parts,
            ..
        } = &entry.kind
        else {
            return None;
        };
        if name.as_ref() != "review" {
            return None;
        }
        parts.iter().find_map(|part| match part {
            JournalPart::Text { text } => Some(text.as_ref()),
            _ => None,
        })
    })
}

fn parent_completion_count(view: &dal_core::View) -> usize {
    view.entries
        .items
        .iter()
        .filter(|entry| matches!(&entry.kind, EntryKind::Assistant { .. }))
        .count()
}

fn parent_completed(view: &dal_core::View) -> bool {
    view.entries.items.iter().any(|entry| {
        let EntryKind::Assistant { content, .. } = &entry.kind else {
            return false;
        };
        content.iter().any(
            |block| matches!(block, Block::Text { text } if text.as_ref() == PARENT_FOLLOW_UP_TEXT),
        )
    })
}

#[tokio::test]
async fn real_git_status_overflow_rejects_before_reviewer_completion_and_closes_cleanly()
-> TestResult {
    let scratch = Scratch::new()?;
    let workspace = create_oversized_status_workspace(&scratch)?;
    let (host, agent, mut subscription) = start_review_session(&scratch, &workspace).await?;
    let session = agent.session();

    let reply = agent
        .submit(SessionCommand::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "review the current changes".into(),
            }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(format!("prompt was not accepted: {reply:?}").into());
    }
    wait_for_turn_end(&agent, &mut subscription).await?;

    let view = agent.view(PageReq::default())?;
    assert_eq!(
        review_error_text(&view),
        Some(STATUS_TOO_LARGE_TEXT),
        "the actual review tool must return the actionable status-cap error"
    );
    assert_eq!(
        parent_completion_count(&view),
        2,
        "the fixture's two completions belong to the parent turn; a reviewer completion would consume the follow-up"
    );
    assert!(
        parent_completed(&view),
        "the parent turn must consume its distinct follow-up completion"
    );
    assert!(
        agent
            .poll_status()
            .await?
            .iter()
            .all(|status| status.ext.as_ref() != "review"),
        "review status must be idle (no live review row) after rejection"
    );

    host.close(session).await?;
    let store = Store::new(
        scratch.path.join("data"),
        Workspace::new(workspace)?,
        dal_core::Product::Dal,
    );
    let (journal, _) = store.open_session(session).await?;
    assert!(
        journal.records().iter().all(|record| {
            !matches!(
                record,
                Record::Ext { ext, kind, .. }
                    if ext.as_ref() == "review" && kind.as_ref() == "review"
            )
        }),
        "the closed parent session must persist no review record"
    );
    Ok(())
}
