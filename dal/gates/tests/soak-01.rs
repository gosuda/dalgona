//! Soak driver: many scripted sessions share one Host and every turn
//! completes inside its budget — the runnable form of the scale contract's
//! responsiveness requirement.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, fmt::Write as _, fs, time::Duration, time::Instant};

use dal_agent::{Agent, Delivery, Env, Host, SessionRef, Subscription};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Expect, Part, Stop, UpdateKind, Workspace,
};
use futures::future::join_all;
use support::TestDir;

const SESSIONS: usize = 8;
const TURNS: usize = 5;
const TURN_BUDGET: Duration = Duration::from_secs(30);

/// One turn: submit a prompt and wait out the `TurnEnded` for it.
///
/// Returns `Ok(true)` on a clean end, `Ok(false)` when the subscription
/// closed first, `Err` when the turn ran past `TURN_BUDGET`.
async fn turn_ended(subscription: &mut Subscription) -> Result<bool, ()> {
    tokio::time::timeout(TURN_BUDGET, async {
        while let Some(delivery) = subscription.next().await {
            if let Delivery::Update(update) = delivery
                && let UpdateKind::TurnEnded {
                    stop: Stop::EndTurn,
                    ..
                } = update.kind
            {
                return true;
            }
        }
        false
    })
    .await
    .map_err(|_| ())
}

/// One session's soak run: `TURNS` sequential turns, returning the worst
/// turn latency. Errors name the session and turn that starved.
async fn soak_session(index: usize, agent: Agent) -> Result<Duration, String> {
    let mut subscription = agent
        .subscribe(None)
        .map_err(|error| format!("session {index} subscribe: {error}"))?;
    let mut worst = Duration::ZERO;
    for turn in 0..TURNS {
        let started = Instant::now();
        agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: format!("soak session {index} turn {turn}").into(),
                }],
            })
            .await
            .map_err(|error| format!("session {index} turn {turn} submit: {error}"))?;
        match turn_ended(&mut subscription).await {
            Ok(true) => worst = worst.max(started.elapsed()),
            Ok(false) => {
                return Err(format!(
                    "session {index} turn {turn}: subscription closed before TurnEnded"
                ));
            }
            Err(()) => {
                return Err(format!(
                    "session {index} turn {turn}: no TurnEnded within {TURN_BUDGET:?}"
                ));
            }
        }
    }
    Ok(worst)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_sessions_complete_every_turn_within_budget()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    // Steps consume in call order across every session, so the fixture needs
    // one events step per (session, turn) pair plus the liveness tail.
    let step = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"soak reply\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":4,\"cached_input_tokens\":0,\"output_tokens\":2,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
    let mut fixture = String::new();
    for _ in 0..=SESSIONS * TURNS {
        let _ = write!(fixture, "{step}");
    }
    let fixture_path = data.path().join("soak.jsonl");
    fs::write(&fixture_path, &fixture)?;

    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\napproval = \"all\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture_path.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    let env = Env {
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let workspace = Workspace::new(workspace.path().to_path_buf())?;

    // All sessions share the one workspace — the contended case the scale
    // contract cares about.
    let mut agents = Vec::new();
    for index in 0..SESSIONS {
        let agent = host
            .open(
                SessionRef::Ephemeral {
                    workspace: workspace.clone(),
                },
                ClientId::new(format!("soak-{index}")),
            )
            .await?;
        agents.push(agent);
    }

    // A starvation defect shows up as one session missing its budget.
    let outcomes = join_all(
        agents
            .into_iter()
            .enumerate()
            .map(|(index, agent)| soak_session(index, agent)),
    )
    .await;
    let mut worst = Duration::ZERO;
    for (index, outcome) in outcomes.iter().enumerate() {
        let session_worst = outcome
            .as_ref()
            .map_err(|error| format!("soak starvation: {error}"))?;
        worst = worst.max(*session_worst);
        assert!(
            *session_worst < TURN_BUDGET,
            "session {index} worst turn {session_worst:?} met the {TURN_BUDGET:?} bound"
        );
    }

    // The Host itself must still answer after the wave: open a fresh session
    // and complete one more turn.
    let agent = host
        .open(
            SessionRef::Ephemeral {
                workspace: workspace.clone(),
            },
            ClientId::new("soak-tail"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "post-soak liveness turn".into(),
            }],
        })
        .await?;
    assert_eq!(
        turn_ended(&mut subscription).await,
        Ok(true),
        "host unresponsive after the soak wave"
    );
    eprintln!("soak: {SESSIONS}x{TURNS} turns, worst {worst:?}");
    Ok(())
}
