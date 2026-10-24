// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The ask battery over a real host: fail-closed without an answerer, the
//! `[ask]` deadline, and the answer round trip. All three are decided by the
//! host's broker machinery, so a scripted `Services` stub cannot stand in
//! for them.

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dal_agent::{Agent, AgentError, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, EntryKind, Expect, JournalPart, PageReq,
    Part, RawJson, Reply, Request, RequestId, UpdateKind, Workspace,
};

const WAIT: Duration = Duration::from_secs(30);
const WAIT_DEADLINE: Duration = Duration::from_secs(180);
const NO_ANSWER: &str = "No answer (dismissed, timed out, or no user attached). Continue on best judgment; do not ask again this turn.";
const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;

const ASK_ARGS: &str = r#"{"questions":[{"id":"pick","header":"Pick","question":"Which one?","kind":"single","options":[{"label":"Alpha"},{"label":"Beta"}]}]}"#;

type TestResult = Result<(), Box<dyn StdError>>;

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Result<Self, Box<dyn StdError>> {
        let path = std::env::temp_dir().join(format!(
            "dalgona-ask-host-{}",
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

fn ask_call_step() -> String {
    "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"ask-call\",\"name\":\"ask\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"ask-call\",\"name\":\"ask\",\"args\":{\"kind\":\"parsed\",\"value\":@ARGS@}}]},@USAGE@,{\"type\":\"stop\",\"reason\":\"tool_use\"}]}"
        .replace("@ARGS@", ASK_ARGS)
        .replace("@USAGE@", USAGE)
}

fn follow_up_step() -> String {
    "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"noted\"},{\"type\":\"tool_calls_done\",\"calls\":[]},@USAGE@,{\"type\":\"stop\",\"reason\":\"end_turn\"}]}"
        .replace("@USAGE@", USAGE)
}

fn config(data: &Path, fixture: &Path, extra: &str) -> Result<Config, Box<dyn StdError>> {
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n[providers.scripted]\nfixture = \"{}\"\n{extra}",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    Ok(Config::load(ConfigProduct::Dalgon, data, "", Some(&user))?)
}

fn env(workspace: &Path) -> Env {
    Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.to_owned(),
        sandbox_helper: None,
    }
}

async fn start_session(
    scratch: &Scratch,
    workspace: &Path,
    extra_config: &str,
) -> Result<(Host, Agent), Box<dyn StdError>> {
    let data = scratch.path.join("data");
    fs::create_dir_all(&data)?;
    let fixture = data.join("script.jsonl");
    fs::write(
        &fixture,
        format!("{}\n{}\n", ask_call_step(), follow_up_step()),
    )?;
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![dalgona_batteries::ask::ask()?],
        bundled: Vec::new(),
    };
    let host = Host::start(
        product,
        config(&data, &fixture, extra_config)?,
        env(workspace),
    )
    .await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace.to_owned())?,
                name: None,
            },
            ClientId::new("ask-host"),
        )
        .await?;
    Ok((host, agent))
}

fn ask_result_text(view: &dal_core::View) -> Option<String> {
    view.entries.items.iter().find_map(|entry| {
        let EntryKind::ToolResult { name, parts, .. } = &entry.kind else {
            return None;
        };
        if name.as_ref() != "ask" {
            return None;
        }
        parts.iter().find_map(|part| match part {
            JournalPart::Text { text } => Some(text.to_string()),
            _ => None,
        })
    })
}

async fn submit_prompt(agent: &Agent) -> TestResult {
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "ask me".into(),
            }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(format!("prompt was not accepted: {reply:?}").into());
    }
    Ok(())
}

/// Runs one ask turn to `TurnEnded`, collecting the opened ask requests.
async fn drive_ask_turn(
    subscription: &mut Subscription,
    wait: Duration,
) -> Result<Vec<Request>, Box<dyn StdError>> {
    let mut requests = Vec::new();
    tokio::time::timeout(wait, async {
        while let Some(delivery) = subscription.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => requests.push(request.clone()),
                UpdateKind::TurnEnded { .. } => return Ok::<(), Box<dyn StdError>>(()),
                _ => {}
            }
        }
        Err::<(), Box<dyn StdError>>("subscription closed before the turn ended".into())
    })
    .await??;
    Ok(requests)
}

/// Answers one request twice: the first answer wins and the losing answer
/// is refused after that resolution.
async fn answer_twice(agent: &Agent, id: RequestId) -> TestResult {
    let value = Answer::Value(RawJson::parse(r#"{"selected":["Alpha"]}"#)?);
    agent.answer(id, value).await?;
    let again = Answer::Value(RawJson::parse(r#"{"selected":["Beta"]}"#)?);
    let second = agent.answer(id, again).await;
    if !matches!(second, Err(AgentError::AlreadyResolved { .. })) {
        return Err(
            format!("the losing answer is refused after the first resolution: {second:?}").into(),
        );
    }
    Ok(())
}

/// With no answering client the real broker fails the question
/// closed to the literal at once instead of waiting or reading stdin.
#[tokio::test]
async fn without_an_answerer_the_broker_fails_closed_to_the_literal() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = scratch.path.join("workspace");
    fs::create_dir_all(&workspace)?;
    let (host, agent) = start_session(&scratch, &workspace, "").await?;
    // A listen-only subscriber never counts as an answerer.
    let mut subscription = agent.subscribe_listen(None)?;
    let session = agent.session();

    let started = std::time::Instant::now();
    submit_prompt(&agent).await?;
    drive_ask_turn(&mut subscription, WAIT).await?;
    let elapsed = started.elapsed();

    let view = agent.view(PageReq::default())?;
    assert_eq!(ask_result_text(&view).as_deref(), Some(NO_ANSWER));
    assert!(
        elapsed < Duration::from_secs(30),
        "no answerer never waits out a deadline: {elapsed:?}"
    );
    host.close(session).await?;
    Ok(())
}

/// The broker fills each request deadline from `[ask] timeout`,
/// and the open question resolves to the merged no-answer literal there.
#[tokio::test]
async fn the_configured_deadline_fails_closed_to_the_literal() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = scratch.path.join("workspace");
    fs::create_dir_all(&workspace)?;
    let (host, agent) = start_session(&scratch, &workspace, "[ask]\ntimeout = \"60s\"\n").await?;
    // An attached answerer holds the question open for the broker deadline.
    let mut subscription = agent.subscribe(None)?;
    let session = agent.session();

    let started = std::time::Instant::now();
    submit_prompt(&agent).await?;
    let requests = drive_ask_turn(&mut subscription, WAIT_DEADLINE).await?;
    let elapsed = started.elapsed();

    let [request] = requests.as_slice() else {
        return Err(format!("expected one open request, saw {}", requests.len()).into());
    };
    assert!(
        request.timeout <= Duration::from_secs(60) && request.timeout >= Duration::from_secs(55),
        "the broker fills the deadline from [ask] timeout: {:?}",
        request.timeout
    );
    let view = agent.view(PageReq::default())?;
    assert_eq!(ask_result_text(&view).as_deref(), Some(NO_ANSWER));
    assert!(
        elapsed >= Duration::from_secs(50),
        "the tool holds the question open until the broker deadline: {elapsed:?}"
    );
    host.close(session).await?;
    Ok(())
}

/// A well-formed value answers through the real broker, and the
/// first answer wins exactly once.
#[tokio::test]
async fn a_broker_answer_round_trip_resolves_exactly_once() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = scratch.path.join("workspace");
    fs::create_dir_all(&workspace)?;
    let (host, agent) = start_session(&scratch, &workspace, "").await?;
    let mut subscription = agent.subscribe(None)?;
    let session = agent.session();

    submit_prompt(&agent).await?;
    let mut resolved = false;
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => {
                    answer_twice(&agent, request.id).await?;
                    resolved = true;
                }
                UpdateKind::TurnEnded { .. } => return Ok::<(), Box<dyn StdError>>(()),
                _ => {}
            }
        }
        Err::<(), Box<dyn StdError>>("subscription closed before the turn ended".into())
    })
    .await??;
    assert!(resolved, "the question opened through the broker");

    let view = agent.view(PageReq::default())?;
    assert_eq!(ask_result_text(&view).as_deref(), Some("Pick: Alpha"));
    host.close(session).await?;
    Ok(())
}

/// The host passes client values through unvalidated, so a
/// wrong-shaped value for its kind still fails closed to the literal.
#[tokio::test]
async fn a_wrong_shaped_broker_value_fails_closed_to_the_literal() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = scratch.path.join("workspace");
    fs::create_dir_all(&workspace)?;
    let (host, agent) = start_session(&scratch, &workspace, "").await?;
    let mut subscription = agent.subscribe(None)?;
    let session = agent.session();

    submit_prompt(&agent).await?;
    let mut answered = false;
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => {
                    agent
                        .answer(request.id, Answer::Value(RawJson::parse("7")?))
                        .await?;
                    answered = true;
                }
                UpdateKind::TurnEnded { .. } => return Ok::<(), Box<dyn StdError>>(()),
                _ => {}
            }
        }
        Err::<(), Box<dyn StdError>>("subscription closed before the turn ended".into())
    })
    .await??;
    assert!(answered, "the question opened through the broker");

    let view = agent.view(PageReq::default())?;
    assert_eq!(ask_result_text(&view).as_deref(), Some(NO_ANSWER));
    host.close(session).await?;
    Ok(())
}
