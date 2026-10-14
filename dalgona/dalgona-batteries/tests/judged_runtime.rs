// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The judged battery opens its session judge against the session's real
//! model identity.
//!
//! A real `Host` runs a directly assembled product with the judged battery
//! and the ask tool over a scripted provider. The judged session setup
//! captures the session's model route, resolved model id, and merged
//! `[judge]` configuration, so the first judged call resolves readiness
//! through the session route and the anchored-ask classifier sees a ready
//! gate. The scripted fixture answers the judge's classification with the
//! first choice, and the proof asserts the `ask` call was blocked by the
//! judged extension with the explore-first reason before the tool ever ran.
//! When the judged opener supplies no session identity, `Judge::open`
//! resolves no route, the gate stays off, the classifier allows the call,
//! and the block assertion fails.

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;
use tokio::fs;

use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, Part, ToolOutcomeView, UpdateKind,
    Workspace,
};
use dalgona_batteries::ask::ask;
use dalgona_batteries::judged::{JudgedConfig, judged};

const WAIT: Duration = Duration::from_secs(30);
const PROMPT_TEXT: &str = "please ask me why the sky is blue";
const ASK_ARGS: &str = r#"{"questions":[{"id":"why_blue","header":"Why","question":"Why is the sky blue?","kind":"text"}]}"#;
/// The judge's reply, with the quotes a JSON string value must carry escaped.
const JUDGE_ANSWERS: &str = r#"{\"answers\":[0]}"#;
const FINAL_TEXT: &str = "resolved without asking.";
const BLOCKED_BY_JUDGED: &str =
    "blocked by judged: ask anchored: the evidence you already have can answer this.";
const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;

type TestResult = Result<(), Box<dyn StdError>>;

/// One model response that requests one non-explicit `ask` call.
fn ask_call_step() -> String {
    [
        r#"{"kind":"events","events":[{"type":"tool_call_started","id":"ask-call","name":"ask"},{"type":"tool_calls_done","calls":[{"id":"ask-call","name":"ask","args":{"kind":"parsed","value":"#,
        ASK_ARGS,
        r"}}]},",
        USAGE,
        r#",{"type":"stop","reason":"tool_use"}]}"#,
    ]
    .concat()
}

/// The judge's classification reply served to the judged model call.
fn judge_answer_step() -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"{JUDGE_ANSWERS}"}},{{"type":"tool_calls_done","calls":[]}},{USAGE},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

/// The post-block continuation response that ends the turn.
fn final_text_step() -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"{FINAL_TEXT}"}},{{"type":"tool_calls_done","calls":[]}},{USAGE},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    async fn new() -> Result<Self, Box<dyn StdError>> {
        let path = std::env::temp_dir().join(format!(
            "dalgona-judged-runtime-{}",
            dal_core::SessionId::new_v7()
        ));
        fs::create_dir_all(&path).await?;
        Ok(Self { path })
    }
}

/// Starts a real host whose judged battery and ask tool share one scripted
/// provider, and opens one fresh session over it.
async fn start_session(
    scratch: &Scratch,
) -> Result<(Host, Agent, Subscription), Box<dyn StdError>> {
    let data = scratch.path.join("data");
    fs::create_dir_all(&data).await?;
    let workspace_dir = scratch.path.join("workspace");
    fs::create_dir_all(&workspace_dir).await?;

    let fixture = data.join("script.jsonl");
    fs::write(
        &fixture,
        format!(
            "{}\n{}\n{}\n",
            ask_call_step(),
            judge_answer_step(),
            final_text_step()
        ),
    )
    .await?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgona, &data, "", Some(&user))?;
    let cfg = JudgedConfig {
        thinking: false,
        ranking: false,
        ask_anchor: true,
        claim_check: false,
        dedup: false,
    };
    let parts = judged(cfg)?;
    let product = Product {
        name: "dalgona",
        data_root: data,
        defaults: "",
        extensions: vec![ask()?, parts.extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace_dir)?,
                name: None,
            },
            ClientId::new("judged-runtime"),
        )
        .await?;
    let subscription = agent.subscribe(None)?;
    Ok((host, agent, subscription))
}

/// Runs the prompt turn and returns the `ask` call's client-visible outcome.
///
/// The turn's only tool call is the scripted `ask`, so the last settled
/// tool outcome is the one under test. Unexpected questions are cancelled so
/// a regression to an unblocked ask still reaches the turn end instead of
/// hanging; the block assertions below carry the verdict.
async fn ask_outcome(
    agent: &Agent,
    subscription: &mut Subscription,
) -> Result<ToolOutcomeView, Box<dyn StdError>> {
    let settled = tokio::time::timeout(WAIT, async {
        let mut settled = None;
        while let Some(delivery) = subscription.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => {
                    agent.answer(request.id, Answer::Cancel).await?;
                }
                UpdateKind::ToolSettled { outcome, .. } => settled = Some(outcome.clone()),
                UpdateKind::TurnEnded { .. } => break,
                _ => {}
            }
        }
        Ok::<_, Box<dyn StdError>>(settled)
    })
    .await
    .map_err(|_| "the turn ends in time")??;
    settled.ok_or_else(|| "the ask call settles in one turn".into())
}

#[tokio::test]
async fn anchored_ask_blocks_through_the_session_judge() -> TestResult {
    let scratch = Scratch::new().await?;
    let (host, agent, mut subscription) = start_session(&scratch).await?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: PROMPT_TEXT.into(),
            }],
        })
        .await?;
    let outcome = ask_outcome(&agent, &mut subscription).await;
    host.close(agent.session()).await?;
    host.shutdown_after_quiet_wait(WAIT).await;
    fs::remove_dir_all(&scratch.path).await?;
    let outcome = outcome?;
    assert!(
        outcome.is_error,
        "the anchored ask is blocked as an error, got {outcome:?}"
    );
    assert!(
        outcome.text.starts_with(BLOCKED_BY_JUDGED),
        "the judged anchor blocks the ask, got {:?}",
        outcome.text
    );
    assert_eq!(
        outcome.elapsed_ms, None,
        "a blocked call never runs the ask tool"
    );
    Ok(())
}
