// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Dalgona product-composition proof for a local judged TTSR rule.
//!
//! These cases use the normal `dalgona::product()` factory through the shared
//! gate support. The unrelated batteries and shipped rule packs are disabled
//! by documented configuration, leaving the product's common assembly and the
//! TTSR extension in place.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use dal_agent::{Delivery, Host, SessionRef, Subscription};
use dal_core::{
    Block, ClientId, Command, EntryKind, Expect, PageReq, Part, Reply, Stop, UpdateKind, View,
    Workspace,
};
use dalgona_gates::support::{self, Scratch, TestResult};

const WAIT: Duration = Duration::from_secs(10);
const PLUGIN_NAME: &str = "local-ttsr";
const RULE_NAME: &str = "local-judged";
const VIOLATION: &str = "XQZ_LOCAL_TTSR_VIOLATION_42";
const RULE_TEXT: &str = "Follow the local correction rule.";
const JUDGE_ON_WITHOUT_CREDENTIALS: &str =
    "dalgon: dal.toml: rules.judge \"on\" needs a judge model with usable credentials.";

const LOCAL_RULE_PLUGIN: &str = r#"load("@dal/v1", "dal")

plugin = dal.plugin(
    name = "local-ttsr",
    version = "0.1.0",
    rules = {
        "local-judged": dal.rule(
            pattern = "XQZ_LOCAL_TTSR_VIOLATION_42",
            text = "Follow the local correction rule.",
            judge = "Does this response violate the local correction rule?",
            interrupt_mode = "always",
        ),
    },
)
"#;

/// The normal product keeps its shared assembly and TTSR extension while the
/// unrelated batteries and compiled rule packs stay out of this proof.
const DISABLED_BATTERIES: &str = "[\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"procs\", \"quality\", \"review\", \"skills\", \"web\", \"work\"]";

fn install_plugin(root: &Path) -> io::Result<()> {
    let directory = root.join("plugins").join(PLUGIN_NAME);
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("plugin.star"), LOCAL_RULE_PLUGIN)
}

fn write_fixture(root: &Path) -> io::Result<PathBuf> {
    let fixture = root.join("local-ttsr.jsonl");
    let first = support::text_step(&format!("partial response {VIOLATION}"));
    // The judge consumes this response through the same scripted provider
    // route. Its text is a JSON object because the judge parser expects the
    // typed bool-answer envelope.
    let judge = r#"{"kind":"events","events":[{"type":"text_delta","text":"{\"answers\":[true]}"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;
    let final_step = support::text_step("corrected response");
    fs::write(&fixture, format!("{first}\n{judge}\n{final_step}\n"))?;
    Ok(fixture)
}

fn user_config(fixture: Option<&Path>, judge_gate: &str) -> String {
    let provider = fixture.map_or_else(String::new, |path| {
        format!(
            "\n[providers.scripted]\nfixture = {:?}\n",
            path.to_string_lossy()
        )
    });
    format!(
        "model = \"openai/gpt-6-luna\"\nplugins = [\"{PLUGIN_NAME}\"]\ndisabled_batteries = {DISABLED_BATTERIES}\n\n[rules]\njudge = \"on\"\nmax_retries = 1\n\n[judge]\ngate = {judge_gate:?}\n\n[rule_sets]\nenabled = []\n{provider}"
    )
}

async fn start_product(root: &Path, fixture: Option<&Path>, judge_gate: &str) -> TestResult<Host> {
    install_plugin(root)?;
    let config = user_config(fixture, judge_gate);
    support::start_dalgona_with_config(root.to_path_buf(), Some(&config)).await
}

async fn open_product_session(
    host: &Host,
    root: &Path,
) -> TestResult<(dal_agent::Agent, Subscription)> {
    let workspace = Workspace::new(root.join("workspace"))?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("dalgona-ttsr-runtime"),
        )
        .await?;
    let subscription = agent.subscribe(None)?;
    Ok((agent, subscription))
}

struct TurnObservation {
    rule_fires: Vec<Box<str>>,
    notices: Vec<String>,
    stop: Stop,
}

async fn await_turn_end(subscription: &mut Subscription) -> TestResult<TurnObservation> {
    let mut rule_fires = Vec::new();
    let mut notices = Vec::new();
    loop {
        let delivery = tokio::time::timeout(WAIT, subscription.next()).await?;
        let Some(Delivery::Update(update)) = delivery else {
            return Err("subscription closed before the turn ended".into());
        };
        match &update.kind {
            UpdateKind::RuleFired { rule, .. } => rule_fires.push(rule.clone()),
            UpdateKind::Notice(notice) => notices.push(format!("{notice:?}")),
            UpdateKind::TurnEnded { stop, .. } => {
                return Ok(TurnObservation {
                    rule_fires,
                    notices,
                    stop: *stop,
                });
            }
            _ => {}
        }
    }
}

fn reminder_entries(view: &View) -> Vec<(&str, &str)> {
    view.entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Reminder { source, text } => Some((source.as_ref(), text.as_ref())),
            _ => None,
        })
        .collect()
}

fn assistant_text(view: &View) -> String {
    view.entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|block| match block {
                        Block::Text { text, .. } => Some(text.as_ref()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

fn journal_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
                files.push(path);
            }
        }
    }
    Ok(files)
}

#[tokio::test]
async fn dalgona_product_enforces_a_local_judged_rule_end_to_end() -> TestResult<()> {
    let scratch = Scratch::new("dalgona-ttsr-runtime")?;
    let root = scratch.path();
    fs::create_dir_all(root.join("workspace"))?;
    let fixture = write_fixture(root)?;
    let host = start_product(root, Some(&fixture), "on").await?;
    let (agent, mut subscription) = open_product_session(&host, root).await?;

    let accepted = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Return the corrected answer after applying the local rule.".into(),
            }],
        })
        .await?;
    assert!(
        matches!(&accepted, Reply::Accepted { .. }),
        "prompt rejected: {accepted:?}"
    );

    let observation = await_turn_end(&mut subscription).await?;
    assert_eq!(
        observation.stop,
        Stop::EndTurn,
        "notices: {:?}",
        observation.notices
    );
    assert_eq!(
        observation
            .rule_fires
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<_>>(),
        [RULE_NAME],
        "the local @dal/v1 rule must publish one RuleFired update"
    );

    let view = agent.view(PageReq::default())?;
    let reminders = reminder_entries(&view);
    assert_eq!(reminders.len(), 1, "the accepted verdict adds one reminder");
    assert_eq!(reminders[0].0, "rule:local-judged");
    assert!(reminders[0].1.contains(RULE_TEXT), "{reminders:?}");
    assert!(
        reminders[0].1.contains("<system-interrupt"),
        "the judged interrupt reminder must be visible: {reminders:?}"
    );
    assert_eq!(assistant_text(&view), "corrected response");

    let session = view.session.id;
    host.close(session).await?;
    let _ = host.shutdown(Duration::from_secs(1)).await;

    let journals = journal_files(root)?;
    assert_eq!(journals.len(), 1, "one durable session journal is expected");
    let journal = fs::read_to_string(&journals[0])?;
    assert!(
        journal
            .lines()
            .any(|line| line.contains("\"type\":\"rule_fired\"")
                && line.contains("\"rule\":\"local-judged\"")),
        "the durable journal must retain the local rule record: {journal}"
    );
    Ok(())
}

#[tokio::test]
async fn required_judge_unavailable_fails_product_session_open_before_inference() -> TestResult<()>
{
    let scratch = Scratch::new("dalgona-ttsr-required-judge")?;
    let root = scratch.path();
    fs::create_dir_all(root.join("workspace"))?;
    let host = start_product(root, None, "off").await?;

    let opened = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(root.join("workspace"))?,
                name: None,
            },
            ClientId::new("dalgona-ttsr-required-judge"),
        )
        .await;
    let Err(error) = opened else {
        let _ = host.shutdown(Duration::from_secs(1)).await;
        return Err("rules.judge = \"on\" unexpectedly opened without a usable judge".into());
    };
    let message = error.to_string();
    assert!(
        message.contains(JUDGE_ON_WITHOUT_CREDENTIALS),
        "required judge startup must fail closed with its product error: {message}"
    );

    let _ = host.shutdown(Duration::from_secs(1)).await;
    for journal in journal_files(root)? {
        let contents = fs::read_to_string(journal)?;
        assert!(
            !contents.contains("\"type\":\"assistant\""),
            "a failed session open must not generate an assistant response: {contents}"
        );
        assert!(
            !contents.contains("\"type\":\"rule_fired\""),
            "a failed session open must not journal a rule fire: {contents}"
        );
    }
    Ok(())
}
