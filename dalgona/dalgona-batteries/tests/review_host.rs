// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The review battery over a real host: a declined `run` grant and a `/review`
//! command clash. Both are decided by the host's grant and registration
//! machinery, so a scripted `Services` stub cannot stand in for them.

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{BoxFuture, CommandCx, CommandHandler, ExtensionBuilder};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    ClientId, Command, CommandName, CommandSpec, Config, ConfigProduct, EntryKind, Expect,
    JournalPart, Origin, Output, PageReq, Part, Reply, ServiceSet, UpdateKind, Workspace,
};
use dalgona_batteries::review::{ReviewConfig, review};

const WAIT: Duration = Duration::from_secs(30);
const PARENT_FOLLOW_UP_TEXT: &str = "parent turn completed";
const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;

type TestResult = Result<(), Box<dyn StdError>>;

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Result<Self, Box<dyn StdError>> {
        let path = std::env::temp_dir().join(format!(
            "dalgona-review-host-{}",
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

fn config(data: &Path, fixture: Option<&Path>) -> Result<Config, Box<dyn StdError>> {
    let scripted = fixture.map_or_else(String::new, |fixture| {
        format!(
            "\n[providers.scripted]\nfixture = \"{}\"\n",
            fixture.to_string_lossy().replace('\\', "\\\\")
        )
    });
    let user = format!("approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n{scripted}");
    Ok(Config::load(ConfigProduct::Dalgon, data, "", Some(&user))?)
}

fn env(workspace: &Path) -> Env {
    Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.to_owned(),
        sandbox_helper: None,
    }
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
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![review(ReviewConfig::default())?],
        bundled: Vec::new(),
    };
    let host = Host::start(product, config(&data, Some(&fixture))?, env(workspace)).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace.to_owned())?,
                name: None,
            },
            ClientId::new("review-host"),
        )
        .await?;
    // A listen-only subscriber observes but cannot answer: no front end can grant.
    let subscription = agent.subscribe_listen(None)?;
    Ok((host, agent, subscription))
}

/// Waits for the turn to end; any question the call opens is a failure.
async fn turn_end_without_questions(subscription: &mut Subscription) -> TestResult {
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => {
                    return Err(
                        format!("a question opened without an answerer: {request:?}").into(),
                    );
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

fn assistant_completions(view: &dal_core::View) -> usize {
    view.entries
        .items
        .iter()
        .filter(|entry| matches!(&entry.kind, EntryKind::Assistant { .. }))
        .count()
}

#[tokio::test]
async fn without_an_answerer_the_run_grant_is_denied_naming_the_run_service() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = scratch.path.join("workspace");
    fs::create_dir_all(&workspace)?;
    let (host, agent, mut subscription) = start_review_session(&scratch, &workspace).await?;
    let session = agent.session();

    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "review the current changes".into(),
            }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(format!("prompt was not accepted: {reply:?}").into());
    }
    turn_end_without_questions(&mut subscription).await?;

    let view = agent.view(PageReq::default())?;
    assert_eq!(
        review_error_text(&view),
        Some(
            "denied: plugin \"review\" has no grant for the \"run\" service; approve the plugin's grant request when it asks, then try again"
        ),
        "a declined grant is the surface Denied form naming the run service"
    );
    assert_eq!(
        assistant_completions(&view),
        2,
        "no reviewer completion runs: the parent turn consumes both scripted steps"
    );
    host.close(session).await?;
    Ok(())
}

struct Rival;

impl CommandHandler for Rival {
    fn run<'a>(
        &'a self,
        _args: &'a str,
        _cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, dal_agent::error::ServiceError>> {
        Box::pin(async { Ok(Reply::Done(Output::Nothing)) })
    }
}

#[tokio::test]
async fn a_user_plugin_claiming_slash_review_fails_the_load() -> TestResult {
    let scratch = Scratch::new()?;
    let data = scratch.path.join("data");
    fs::create_dir_all(&data)?;
    let rival = ExtensionBuilder::new("rival", "0.1.0", ServiceSet::default())?
        .with_origin(Origin::User, None)
        .command(
            CommandSpec {
                name: CommandName::parse("review")?,
                summary: "A rival review.".into(),
                args_hint: None,
            },
            Arc::new(Rival),
        )
        .build()?;
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![review(ReviewConfig::default())?, rival],
        bundled: Vec::new(),
    };

    let failure = Host::start(product, config(&data, None)?, env(&scratch.path))
        .await
        .err()
        .ok_or("the host started with two claimants of /review")?;

    assert_eq!(
        failure.to_string(),
        "command \"review\" is already registered by plugin \"review\"; plugin \"rival\" cannot register it"
    );
    Ok(())
}
