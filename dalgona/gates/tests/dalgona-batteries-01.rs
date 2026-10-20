// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! History letters round-trip: stored PNG bytes and exact letter text.
use std::path::Path;
use std::time::Duration;

use dal_agent::{Agent, Delivery, Host, SessionRef, Subscription};
use dal_core::{
    Command, EntryKind, Expect, JournalPart, PageReq, Part, Stop, UpdateKind, Workspace,
};

use dalgona_gates::support::{self, TestResult};

const WAIT: Duration = Duration::from_secs(30);

/// One scripted turn. All strings used here are fixed, JSON-safe fixture text.
fn text_step(text: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"{text}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":40000,\"cached_input_tokens\":0,\"output_tokens\":10,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}"
    )
}

/// A scripted call through the real read tool, followed by a completed reply.
fn read_steps(path: &str) -> [String; 2] {
    [
        format!(
            "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"read-1\",\"name\":\"read\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"read-1\",\"name\":\"read\",\"args\":{{\"kind\":\"parsed\",\"value\":{{\"path\":\"{path}\"}}}}}}]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":40000,\"cached_input_tokens\":0,\"output_tokens\":10,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}"
        ),
        text_step("read done"),
    ]
}

async fn image_host(root: &Path, steps: &[String]) -> TestResult<Host> {
    let script = root.join("script.jsonl");
    std::fs::write(&script, steps.join("\n"))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\ndisabled_batteries = [\"ask\", \"judged\", \"mcp\", \"orchestration\", \"procs\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]\n[plugin.history]\nshare = 0.4\n[providers.scripted]\nfixture = {:?}\n",
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

async fn prompt(agent: &Agent, subscription: &mut Subscription, text: &str) -> TestResult<()> {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    while let Some(delivery) = tokio::time::timeout(WAIT, subscription.next()).await? {
        if let Delivery::Update(update) = delivery
            && let UpdateKind::TurnEnded { stop, .. } = &update.kind
        {
            assert_eq!(*stop, Stop::EndTurn, "the scripted turn ends cleanly");
            return Ok(());
        }
    }
    Err("subscription closed before the turn ended".into())
}

async fn compact(agent: &Agent, subscription: &mut Subscription) -> TestResult<Box<str>> {
    agent.submit(Command::Compact { focus: None }).await?;
    while let Some(delivery) = tokio::time::timeout(WAIT, subscription.next()).await? {
        if let Delivery::Update(update) = delivery
            && let UpdateKind::Notice(notice) = &update.kind
            && matches!(
                notice.kind.as_ref(),
                "compaction_ended" | "compact.manual_failed"
            )
        {
            return Ok(notice.text.clone());
        }
    }
    Err("subscription closed before the compaction ended".into())
}

fn compaction_parts(agent: &Agent) -> TestResult<Vec<JournalPart>> {
    let mut before = None;
    loop {
        let view = agent.view(PageReq {
            limit: std::num::NonZeroU32::new(PageReq::MAX_LIMIT).ok_or("page limit")?,
            before,
        })?;
        if let Some(parts) =
            view.entries
                .items
                .into_iter()
                .rev()
                .find_map(|entry| match entry.kind {
                    EntryKind::Compaction { parts, .. } => Some(parts),
                    _ => None,
                })
        {
            return Ok(parts);
        }
        let Some(next) = view.entries.next_before else {
            return Err("compaction did not produce a journal entry".into());
        };
        before = Some(next);
    }
}

/// Returns the newest read result, not the rest of the evolving session view.
fn read_result(agent: &Agent) -> TestResult<String> {
    let view = agent.view(PageReq::default())?;
    let result = view
        .entries
        .items
        .into_iter()
        .rev()
        .find_map(|entry| match entry.kind {
            EntryKind::ToolResult { parts, .. } => Some(parts),
            _ => None,
        })
        .ok_or("read did not produce a tool result")?;
    Ok(format!("{result:?}"))
}

#[tokio::test]
async fn history_images_round_trip_through_letter_scheme() -> TestResult<()> {
    let scratch = support::Scratch::new("history-letter")?;
    let root = scratch.path();
    std::fs::create_dir_all(root.join("workspace"))?;
    let long = "the quick brown fox jumps over the lazy dog. ".repeat(700);
    let mut steps: Vec<String> = (0..4).map(|_| text_step("reply")).collect();
    steps.push(text_step("remote attempt"));
    steps.extend(read_steps("letter://history/1.1"));
    steps.extend(read_steps("letter://"));
    let host = image_host(root, &steps).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(root.join("workspace"))?,
                name: Some("history-letter".into()),
            },
            dal_core::ClientId::new("history-letter-gate"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    for _ in 0..4 {
        prompt(&agent, &mut subscription, &long).await?;
    }
    let notice = compact(&agent, &mut subscription).await?;
    assert!(
        notice.contains("by history"),
        "history must commit before summary: {notice}"
    );
    // The committed compaction carries its images inline behind the labels,
    // and each PNG is stored under its digest through its letter record.
    let parts = compaction_parts(&agent)?;
    let stored = parts.iter().any(|part| {
        matches!(
            part,
            JournalPart::Image { mime, base64 }
                if mime.as_ref() == "image/png" && !base64.is_empty()
        )
    });
    assert!(stored, "history stores image bytes: {parts:?}");
    assert!(
        parts.windows(2).any(|pair| matches!(
            pair,
            [JournalPart::Text { text }, JournalPart::Image { .. }]
                if text.as_ref() == "letter://history/1.1"
        )),
        "the image is labeled with its letter id: {parts:?}"
    );
    // The letter scheme resolves the stored text exactly, not the manual.
    prompt(&agent, &mut subscription, "read the letter").await?;
    let first_letter = read_result(&agent)?;
    assert!(
        first_letter.contains("the quick brown fox"),
        "stored text remains exact text in the letter: {first_letter}"
    );
    assert!(
        !first_letter.contains("missing journal"),
        "the stored text blob is available"
    );
    // The letter index lists the committed id.
    prompt(&agent, &mut subscription, "read the letter index").await?;
    let index = read_result(&agent)?;
    assert!(
        index.contains("history/1."),
        "the index lists the stored letter: {index}"
    );
    let key = agent.view(PageReq::default())?.session.id;
    host.close(key).await?;
    drop(subscription);
    drop(agent);
    host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}
