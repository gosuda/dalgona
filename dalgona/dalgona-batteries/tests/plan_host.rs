// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The plan battery over a real host: the `work` extension's structured
//! status payload and its human line describe the same observed plan phase,
//! legal cap-size todo lists written through the real tool path are
//! represented within the 4096-byte status bound, and an explicit `/plan
//! off` returns the kind to the wire baseline. Decided by the host's
//! registration, journal, tool dispatch, and status sweep, so a scripted
//! `Services` stub cannot stand in for them.

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, ExtState, Part, Reply, UpdateKind, Workspace,
};
use dalgona_batteries::work::{PlanConfig, work};

const WAIT: Duration = Duration::from_secs(30);
const STATUS_TRUNCATION_SUFFIX: &str = "[...] status truncated";

type TestResult = Result<(), Box<dyn StdError>>;

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Result<Self, Box<dyn StdError>> {
        let path = std::env::temp_dir().join(format!(
            "dalgona-plan-host-{}",
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

fn end_turn_step() -> String {
    r#"{"kind":"events","events":[{"type":"text_delta","text":"todos written"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#.to_owned()
}

fn todo_write_step(args: &str) -> Result<String, Box<dyn StdError>> {
    let parsed: sonic_rs::Value = sonic_rs::from_str(args)?;
    let step = sonic_rs::json!({
        "kind": "events",
        "events": [
            {
                "type": "tool_call_started",
                "id": "todo-write",
                "name": "todo",
            },
            {
                "type": "tool_calls_done",
                "calls": [
                    {
                        "id": "todo-write",
                        "name": "todo",
                        "args": {"kind": "parsed", "value": parsed},
                    }
                ],
            },
            {
                "type": "usage",
                "usage": {
                    "input_tokens": 10,
                    "cached_input_tokens": 0,
                    "output_tokens": 5,
                    "reasoning_tokens": null,
                    "cache_write_tokens": 0,
                    "cost_usd": null,
                },
            },
            {
                "type": "stop",
                "reason": "tool_use",
            },
        ]
    });
    Ok(sonic_rs::to_string(&step)?)
}

/// 26 legal subjects at the 200-byte cap: escaped quotes plus multibyte CJK,
/// so escaping pushes the verbatim status payload past 4096 bytes while the
/// tool input stays far below the 16 KiB tool input cap.
fn todo_write_args() -> Result<String, Box<dyn StdError>> {
    let subject = format!("{}{}", "\"".repeat(50), "\u{754c}".repeat(50));
    let encoded = sonic_rs::to_string(&subject)?;
    let item = format!(r#"{{"subject":{encoded},"state":"pending"}}"#);
    let args = format!(
        r#"{{"action":"write","todos":[{}]}}"#,
        std::iter::repeat_n(item.as_str(), 26)
            .collect::<Vec<_>>()
            .join(",")
    );
    Ok(args)
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

async fn start_plan_session(
    scratch: &Scratch,
    workspace: &Path,
    steps: &[String],
) -> Result<(Host, Agent, Subscription), Box<dyn StdError>> {
    let data = scratch.path.join("data");
    fs::create_dir_all(&data)?;
    let fixture = data.join("script.jsonl");
    fs::write(&fixture, steps.join("\n") + "\n")?;
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![work(PlanConfig::parse_config(None)?)?],
        bundled: Vec::new(),
    };
    let host = Host::start(product, config(&data, Some(&fixture))?, env(workspace)).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace.to_owned())?,
                name: None,
            },
            ClientId::new("plan-host"),
        )
        .await?;
    let subscription = agent.subscribe_listen(None)?;
    Ok((host, agent, subscription))
}

async fn turn_end(subscription: &mut Subscription) -> TestResult {
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            if matches!(update.kind, UpdateKind::TurnEnded { .. }) {
                return Ok(());
            }
        }
        Err("subscription closed before the parent turn ended".into())
    })
    .await?
}

async fn run_plan(agent: &Agent, args: &str) -> Result<(), Box<dyn StdError>> {
    let reply = agent
        .submit(Command::Run {
            name: "plan".into(),
            args: args.into(),
            expected: None,
        })
        .await?;
    assert!(
        matches!(reply, Reply::Done(_)),
        "the /plan command was not accepted: {reply:?}"
    );
    Ok(())
}

#[derive(serde::Deserialize)]
struct PayloadTodo {
    subject: String,
    state: String,
}

#[derive(serde::Deserialize)]
struct StatusPayload {
    plan_mode: bool,
    plan_state: String,
    todos: Vec<PayloadTodo>,
}

fn work_row(statuses: &[dal_core::ExtStatus]) -> Result<&dal_core::ExtStatus, Box<dyn StdError>> {
    statuses
        .iter()
        .find(|status| status.ext.as_ref() == "work")
        .ok_or_else(|| "the work status row is missing".into())
}

#[tokio::test]
async fn plan_status_payload_and_human_line_track_the_same_phase() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = scratch.path.join("workspace");
    fs::create_dir_all(&workspace)?;
    let (host, agent, _subscription) =
        start_plan_session(&scratch, &workspace, &[end_turn_step()]).await?;
    let session = agent.session();

    run_plan(&agent, "on").await?;
    let statuses = agent.poll_status().await?;
    let work = work_row(&statuses)?;
    assert_eq!(
        work.state,
        ExtState::Quiet,
        "planning is quiet: only an awaiting review is busy"
    );
    assert_eq!(
        work.text.as_deref(),
        Some("planning"),
        "the human line names the planning phase"
    );
    let Some(payload) = work.payload.as_ref() else {
        return Err("the planning status carries no structured payload".into());
    };
    assert!(payload.as_str().len() <= 4096);
    let decoded: StatusPayload = sonic_rs::from_str(payload.as_str())?;
    assert!(decoded.plan_mode);
    assert_eq!(decoded.plan_state, "planning");
    assert!(decoded.todos.is_empty());

    run_plan(&agent, "off").await?;
    assert!(
        agent
            .poll_status()
            .await?
            .iter()
            .all(|status| status.ext.as_ref() != "work"),
        "an explicit off returns the work kind to the wire baseline"
    );
    host.close(session).await?;
    Ok(())
}

#[tokio::test]
async fn plan_status_payload_represents_the_legal_cap_todos() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = scratch.path.join("workspace");
    fs::create_dir_all(&workspace)?;
    let steps = [todo_write_step(&todo_write_args()?)?, end_turn_step()];
    let (host, agent, mut subscription) = start_plan_session(&scratch, &workspace, &steps).await?;
    let session = agent.session();

    run_plan(&agent, "on").await?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "write the plan todos".into(),
            }],
        })
        .await?;
    assert!(
        matches!(reply, Reply::Accepted { .. }),
        "the todo write prompt was not accepted: {reply:?}"
    );
    turn_end(&mut subscription).await?;

    let statuses = agent.poll_status().await?;
    let work = work_row(&statuses)?;
    assert_eq!(
        work.text.as_deref(),
        Some("planning \u{b7} 0/26 done"),
        "the human line keeps the counts next to the planning phase"
    );
    let Some(payload) = work.payload.as_ref() else {
        return Err("the todo status carries no structured payload".into());
    };
    assert!(
        payload.as_str().len() <= 4096,
        "the structured payload stays within the status bound"
    );
    let decoded: StatusPayload = sonic_rs::from_str(payload.as_str())?;
    assert!(decoded.plan_mode);
    assert_eq!(decoded.plan_state, "planning");
    assert_eq!(decoded.todos.len(), 26, "no todo is dropped");
    assert!(decoded.todos.iter().all(|todo| todo.state == "pending"
        && (todo.subject.ends_with(STATUS_TRUNCATION_SUFFIX)
            || todo.subject.contains('\u{754c}'))));
    assert!(
        decoded
            .todos
            .iter()
            .any(|todo| todo.subject.ends_with(STATUS_TRUNCATION_SUFFIX))
    );

    run_plan(&agent, "off").await?;
    host.close(session).await?;
    Ok(())
}
