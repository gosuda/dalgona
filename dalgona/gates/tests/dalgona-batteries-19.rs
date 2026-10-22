// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! History image compaction through the real product, journal, restart, and read tool.
use std::path::Path;
use std::time::Duration;

use dal_agent::{Agent, Delivery, Host, SessionRef, Subscription};
use dal_core::{
    Command, EntryKind, EntryView, Expect, JournalPart, PageReq, Part, Stop, UpdateKind, Workspace,
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
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\ndisabled_batteries = [\"ask\", \"judged\", \"mcp\", \"orchestration\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]\n[plugin.history]\nshare = 0.4\n[providers.scripted]\nfixture = {:?}\n",
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

async fn session(host: &Host, reference: SessionRef) -> TestResult<(Agent, Subscription)> {
    let agent = host
        .open(reference, dal_core::ClientId::new("history-gate"))
        .await?;
    let subscription = agent.subscribe(None)?;
    Ok((agent, subscription))
}

/// The first `limit` characters of `text`, for a failure message.
fn brief(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// One prompt turn. `stage` names the phase and prompt, so a failure
/// identifies which scripted turn ended badly; the failure message also
/// carries the notices (a failed turn publishes its typed `turn.failed`
/// reason as one) and tool events the subscription saw, then the newest
/// entry kinds and assistant content, which together show which scripted
/// step the turn took.
async fn prompt(
    agent: &Agent,
    subscription: &mut Subscription,
    stage: &str,
    text: &str,
) -> TestResult<()> {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let next = tokio::time::timeout(WAIT, subscription.next())
            .await
            .map_err(|_| {
                format!(
                    "{stage}: no update within {WAIT:?}; events: {}",
                    seen.join(" | ")
                )
            })?;
        let Some(delivery) = next else {
            return Err(format!(
                "{stage}: subscription closed before the turn ended; events: {}",
                seen.join(" | ")
            )
            .into());
        };
        let Delivery::Update(update) = delivery else {
            seen.push("resync".into());
            continue;
        };
        if let UpdateKind::TurnEnded { stop, .. } = &update.kind {
            assert_eq!(
                *stop,
                Stop::EndTurn,
                "{stage} (prompt {:?}): the scripted turn ends cleanly; events: {}; {}",
                brief(text, 40),
                seen.join(" | "),
                view_trail(agent)
            );
            assert!(
                !seen
                    .iter()
                    .any(|event| event.starts_with("notice compaction_started")),
                "{stage}: no automatic compaction runs; the script has no step for one; events: {}",
                seen.join(" | ")
            );
            return Ok(());
        }
        seen.extend(describe(&update.kind));
    }
}

/// The update as a failure-message fragment, when it explains how a turn went.
fn describe(kind: &UpdateKind) -> Option<String> {
    match kind {
        UpdateKind::Notice(notice) => Some(format!(
            "notice {}: {}",
            notice.kind,
            brief(&notice.text, 240)
        )),
        UpdateKind::ToolStarted { tool, .. } => Some(format!("tool_started {tool}")),
        UpdateKind::ToolSettled { outcome, .. } => Some(format!(
            "tool_settled {}",
            brief(&format!("{outcome:?}"), 160)
        )),
        _ => None,
    }
}

/// The session's newest entry kinds and assistant content, or why its view
/// could not be read.
fn view_trail(agent: &Agent) -> String {
    match agent.view(PageReq::default()) {
        Ok(view) => trail(&view.entries.items),
        Err(error) => format!("session view unavailable: {error}"),
    }
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

fn kind_name(kind: &EntryKind) -> &'static str {
    match kind {
        EntryKind::User { .. } => "user",
        EntryKind::Assistant { .. } => "assistant",
        EntryKind::ToolResult { .. } => "tool_result",
        EntryKind::Compaction { .. } => "compaction",
        _ => "other",
    }
}

/// The kinds of the newest entries, oldest first, and the start of the newest
/// assistant content: which scripted step the newest turn took.
fn trail(items: &[EntryView]) -> String {
    let kinds: Vec<&str> = items.iter().map(|entry| kind_name(&entry.kind)).collect();
    let newest = kinds[kinds.len().saturating_sub(12)..].join(",");
    let reply = items
        .iter()
        .rev()
        .find_map(|entry| match &entry.kind {
            EntryKind::Assistant { content, .. } => Some(brief(&format!("{content:?}"), 120)),
            _ => None,
        })
        .unwrap_or_default();
    format!("newest entries: {newest}; newest assistant: {reply}")
}

/// Returns the newest read result, not the rest of the evolving session view.
///
/// `call` names the phase and read, so a failure identifies its stage; the
/// error also carries the kinds of the newest entries, oldest first, and the
/// start of the newest assistant content, which together show whether the turn
/// ran a tool call at all and which scripted step it took.
fn read_result(agent: &Agent, call: &str) -> TestResult<String> {
    let view = agent.view(PageReq::default())?;
    let recent = trail(&view.entries.items);
    let result = view
        .entries
        .items
        .into_iter()
        .rev()
        .find_map(|entry| match entry.kind {
            EntryKind::ToolResult { parts, .. } => Some(parts),
            _ => None,
        })
        .ok_or_else(|| format!("{call}: read did not produce a tool result; {recent}"))?;
    Ok(format!("{result:?}"))
}

fn image_parts(agent: &Agent) -> TestResult<Vec<JournalPart>> {
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

fn text_parts(parts: &[JournalPart]) -> Vec<String> {
    parts
        .iter()
        .filter_map(|part| match part {
            JournalPart::Text { text } => Some(text.to_string()),
            _ => None,
        })
        .collect()
}

fn first_image(parts: &[JournalPart]) -> Option<JournalPart> {
    parts.windows(2).find_map(|pair| match pair {
        [
            JournalPart::Text { text },
            image @ JournalPart::Image { .. },
        ] if text.as_ref() == "letter://history/1.1" => Some(image.clone()),
        _ => None,
    })
}

async fn create_history_image(root: &Path, long: &str) -> TestResult<(Vec<JournalPart>, String)> {
    let mut steps: Vec<String> = (0..4).map(|_| text_step("reply")).collect();
    steps.push(text_step("remote attempt"));
    steps.extend(read_steps("letter://history/1.1"));
    let host = image_host(root, &steps).await?;
    let (agent, mut subscription) = session(
        &host,
        SessionRef::New {
            workspace: Workspace::new(root.join("workspace"))?,
            name: Some("history-images".into()),
        },
    )
    .await?;
    for round in 1..=4 {
        let stage = format!("create phase: long prompt {round}");
        prompt(&agent, &mut subscription, &stage, long).await?;
    }
    let notice = compact(&agent, &mut subscription).await?;
    assert!(
        notice.contains("by history"),
        "history must commit before summary: {notice}"
    );
    let parts = image_parts(&agent)?;
    assert!(
        parts.iter().any(|part| matches!(
            part,
            JournalPart::ImageBlob { .. } | JournalPart::Image { .. }
        )),
        "compaction must return image parts"
    );
    prompt(
        &agent,
        &mut subscription,
        "create phase: read letter after the first compaction",
        "read the letter",
    )
    .await?;
    let first_letter = read_result(&agent, "create phase: read letter://history/1.1")?;
    assert!(
        first_letter.contains("the quick brown fox"),
        "stored text remains exact text in the letter: {first_letter}"
    );
    assert!(
        !first_letter.contains("missing journal"),
        "the stored text blob is available"
    );
    assert_eq!(
        image_parts(&agent)?,
        parts,
        "reading a letter keeps the compaction on the branch"
    );
    let key = agent.view(PageReq::default())?.session.id;
    host.close(key).await?;
    drop(subscription);
    drop(agent);
    host.shutdown(Duration::from_secs(2)).await;
    Ok((parts, first_letter))
}

async fn resume_history_image(
    root: &Path,
    long: &str,
    parts: &[JournalPart],
    first_letter: &str,
) -> TestResult<()> {
    let mut steps = Vec::from(read_steps("letter://history/1.1"));
    steps.extend((0..4).map(|_| text_step("reply")));
    steps.push(text_step("remote attempt"));
    steps.extend(read_steps("letter://"));
    let host = image_host(root, &steps).await?;
    let (agent, mut subscription) = session(
        &host,
        SessionRef::Resume {
            workspace: Workspace::new(root.join("workspace"))?,
            key: "history-images".into(),
        },
    )
    .await?;
    let resumed_parts = image_parts(&agent)?;
    assert_eq!(
        text_parts(&resumed_parts),
        text_parts(parts),
        "resume preserves image labels and part order"
    );
    assert!(
        resumed_parts
            .iter()
            .any(|part| matches!(part, JournalPart::ImageBlob { bytes, .. } if *bytes > 0)),
        "resume replays stored image blobs"
    );
    prompt(
        &agent,
        &mut subscription,
        "resume phase: read stored letter after restart",
        "read the stored letter",
    )
    .await?;
    assert_eq!(
        read_result(
            &agent,
            "resume phase: read letter://history/1.1 after restart"
        )?,
        first_letter,
        "the letter text is byte-equal after restart"
    );
    for round in 1..=4 {
        let stage = format!("resume phase: long prompt {round}");
        prompt(&agent, &mut subscription, &stage, long).await?;
    }
    let notice = compact(&agent, &mut subscription).await?;
    assert!(
        notice.contains("by history"),
        "second compaction commits images: {notice}"
    );
    let second_parts = image_parts(&agent)?;
    let reused =
        first_image(&second_parts).ok_or("the first known letter was not reused as an image")?;
    assert_eq!(
        Some(reused),
        first_image(parts),
        "known letters keep the same label and PNG bytes"
    );
    prompt(
        &agent,
        &mut subscription,
        "resume phase: read letter index after the second compaction",
        "read the letter index",
    )
    .await?;
    let index = read_result(
        &agent,
        "resume phase: read letter:// index after the second compaction",
    )?;
    assert!(
        index.contains("history/1."),
        "the old letters are reused: {index}"
    );
    assert!(
        index.contains("history/2."),
        "new history has a fresh ordinal: {index}"
    );
    let key = agent.view(PageReq::default())?.session.id;
    host.close(key).await?;
    host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}

#[tokio::test]
async fn history_commits_image_parts_letters_and_resume_replays_them() -> TestResult<()> {
    let scratch = support::Scratch::new("history-images")?;
    let root = scratch.path();
    std::fs::create_dir_all(root.join("workspace"))?;
    let long = "the quick brown fox jumps over the lazy dog. ".repeat(700);
    let (parts, first_letter) = create_history_image(root, &long).await?;
    resume_history_image(root, &long, &parts, &first_letter).await
}
