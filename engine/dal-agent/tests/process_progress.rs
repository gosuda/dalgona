//! Process progress boundary: while a real child runs, the session must
//! publish its last 20 output lines as `ToolProgress` rows keyed by the
//! running call; ticks stop at the terminal outcome and at cancellation.
//!
//! Both tests drive the real spawn door through `Host` with a controlled
//! `sh` child and observe the session subscription. The positive
//! assertions are gated on output markers the child prints, never on
//! sleeps. The 700 ms post-end drains only bound the negative absence
//! checks.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dal_agent::ext::{
    ArgError, BoxFuture, ExportSpec, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx,
    ToolOutcome, ToolOutput,
};
use dal_agent::{Env, Host, Product, SessionRef, SpawnOpts, ToolError};
use dal_core::ext::{ExportId, ExportKind, OpSet};
use dal_core::{
    CallId, CancelScope, ClientId, Command, Config, ConfigProduct, Expect, ModelInfo, Name, Part,
    Preview, RawJson, Stop, ToolClass, ToolSpec, TurnId, UpdateKind, Visibility, Workspace,
};

const STEP_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"probe-call\",\"name\":\"fixture__progress\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"probe-call\",\"name\":\"fixture__progress\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";

const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// A model-visible tool that spawns the rig script through the same
/// checked door the exec tool uses and records its call identity so the
/// tests can assert the progress key.
struct ProgressProbe {
    name: Name,
    spec: Arc<ToolSpec>,
    script: String,
    seen: Arc<tokio::sync::Mutex<Option<CallId>>>,
}

impl Tool for ProgressProbe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Exec {
            read_only: false,
            grant: None,
        })
    }

    fn run<'a>(&'a self, call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            *self.seen.lock().await = Some(call.id.clone());
            let preview = Preview {
                title: "progress-probe".into(),
                body: self.script.clone().into(),
                digest: None,
            };
            let approved = match cx.authorize(preview).await {
                Ok(approved) => approved,
                Err(reason) => return ToolOutcome::Err(ToolError::Denied(reason)),
            };
            let argv = [
                OsString::from("sh"),
                OsString::from("-c"),
                OsString::from(&self.script),
            ];
            let opts = SpawnOpts {
                cwd: cx.workspace().as_path().to_path_buf(),
                timeout: None,
                env: Vec::new(),
                stdout_prefix_limit: 0,
            };
            let mut proc = match cx.spawn(&argv, opts, approved) {
                Ok(proc) => proc,
                Err(error) => return ToolOutcome::Err(error),
            };
            match proc.wait(cx.cancel()).await {
                Ok(result) => ToolOutcome::Ok(Box::new(ToolOutput::from_text(format!(
                    "probe exit {status:?}",
                    status = result.status
                )))),
                Err(error) => ToolOutcome::Err(error),
            }
        })
    }
}

struct Rig {
    agent: dal_agent::Agent,
    subscription: dal_agent::Subscription,
    workspace_dir: PathBuf,
    _tmp: tempfile::TempDir,
    seen: Arc<tokio::sync::Mutex<Option<CallId>>>,
    turn: TurnId,
}

/// Opens one session on `host`, prompts the scripted turn, and returns
/// the live agent, its subscription, and the turn the probe runs under.
async fn open_probe_session(
    host: &Host,
    workspace_dir: &Path,
) -> Result<(dal_agent::Agent, dal_agent::Subscription, TurnId), Box<dyn std::error::Error>> {
    let workspace = Workspace::new(workspace_dir.to_path_buf())?;
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await?;
    let subscription = agent.subscribe(None)?;
    // The fold starts at the ask policy; the probe must run without a
    // front end so the spawn door itself is what the test observes.
    let _ = agent
        .submit(Command::SetApproval {
            mode: dal_core::ApprovalMode::All,
            save: dal_core::Save::SessionOnly,
        })
        .await?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "spawn the probe".into(),
            }],
        })
        .await?;
    let dal_core::Reply::Accepted { turn, .. } = reply else {
        return Err(format!("prompt not accepted: {reply:?}").into());
    };
    Ok((agent, subscription, turn))
}

/// Starts one host with the probe tool registered, runs one turn whose
/// provider step calls it, and returns the live subscription before the
/// child finishes.
async fn start_rig(script: &str) -> Result<Rig, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, format!("{STEP_CALL}{STEP_END}"))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str()))?;
    let seen = Arc::new(tokio::sync::Mutex::new(None));
    let probe = Arc::new(ProgressProbe {
        name: Name::parse("fixture__progress")?,
        spec: Arc::new(ToolSpec {
            name: Name::parse("fixture__progress")?,
            description: "progress probe tool".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#)?,
            grammar: None,
        }),
        script: script.to_owned(),
        seen: Arc::clone(&seen),
    });
    let extension = ExtensionBuilder::new("fixture", "0.1.0", dal_core::ServiceSet::EMPTY)?
        .script_tool(
            probe,
            Visibility::Model,
            ExportSpec {
                id: ExportId {
                    plugin: Name::parse("fixture")?,
                    kind: ExportKind::Tool,
                    local: Name::parse("progress")?,
                },
                uses: OpSet::EMPTY,
                input: RawJson::parse(r#"{"type":"object"}"#)?,
                description: "progress probe export".into(),
            },
        )
        .build()?;
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (
                OsString::from("HOME"),
                OsString::from(tmp.path().join("home")),
            ),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(tmp.path().join("cache")),
            ),
        ]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let (agent, subscription, turn) = open_probe_session(&host, &workspace_dir).await?;
    Ok(Rig {
        agent,
        subscription,
        workspace_dir,
        _tmp: tmp,
        seen,
        turn,
    })
}

/// Waits for the probe tool to record its call identity.
async fn tool_call_id(
    seen: &Arc<tokio::sync::Mutex<Option<CallId>>>,
) -> Result<CallId, Box<dyn std::error::Error>> {
    for _ in 0..100 {
        if let Some(call) = seen.lock().await.clone() {
            return Ok(call);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err("the probe never ran".into())
}

/// Reads one live delivery, or reports a stalled session.
async fn next_delivery(
    subscription: &mut dal_agent::Subscription,
) -> Result<dal_agent::Delivery, Box<dyn std::error::Error>> {
    let delivery = tokio::time::timeout(Duration::from_secs(10), subscription.next()).await;
    let Ok(Some(delivery)) = delivery else {
        return Err("the subscription stalled".into());
    };
    Ok(delivery)
}

/// The progress text when `delivery` is a progress row for `call`.
fn progress_for(delivery: &dal_agent::Delivery, call: &CallId) -> Option<String> {
    let dal_agent::Delivery::Update(update) = delivery else {
        return None;
    };
    let UpdateKind::ToolProgress {
        call: row_call,
        tail,
    } = &update.kind
    else {
        return None;
    };
    (row_call == call).then(|| tail.to_string())
}

/// Whether `delivery` ends the turn, and whether the end was a cancellation.
fn turn_end_cancelled(delivery: &dal_agent::Delivery) -> Option<bool> {
    let dal_agent::Delivery::Update(update) = delivery else {
        return None;
    };
    let UpdateKind::TurnEnded { stop, .. } = &update.kind else {
        return None;
    };
    Some(*stop == Stop::Cancelled)
}

/// Drains progress rows for `call` until one carries `marker`, returning
/// every row seen for the call and the marked row.
async fn progress_rows_until(
    subscription: &mut dal_agent::Subscription,
    call: &CallId,
    marker: &str,
) -> Result<(Vec<String>, String), Box<dyn std::error::Error>> {
    let mut rows = Vec::new();
    for _ in 0..40 {
        let delivery = next_delivery(subscription).await?;
        if let Some(text) = progress_for(&delivery, call) {
            if text.contains(marker) {
                rows.push(text.clone());
                return Ok((rows, text));
            }
            rows.push(text);
        }
    }
    Err(format!("no progress row for the call carried {marker:?}").into())
}

/// Drains until the turn ends, returning whether it ended cancelled.
async fn drain_until_end(
    subscription: &mut dal_agent::Subscription,
) -> Result<bool, Box<dyn std::error::Error>> {
    for _ in 0..40 {
        let delivery = next_delivery(subscription).await?;
        if let Some(cancelled) = turn_end_cancelled(&delivery) {
            return Ok(cancelled);
        }
    }
    Err("the turn never ended".into())
}

/// Fails if any further progress row for `call` arrives within the
/// bound; silence here is the stop-at-terminal evidence.
async fn assert_progress_quiet(
    subscription: &mut dal_agent::Subscription,
    call: &CallId,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_millis(700);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        let Ok(Some(delivery)) = tokio::time::timeout(remaining, subscription.next()).await else {
            return Ok(());
        };
        if progress_for(&delivery, call).is_some() {
            return Err("progress arrived after the terminal outcome".into());
        }
    }
}

/// A running child publishes exactly the last 20 of its 25 lines under
/// the running call's identity, once, and nothing after the exit.
#[tokio::test]
async fn running_child_publishes_last_twenty_lines_keyed_by_call() {
    let script = "printf 'L%02d\\n' $(seq 1 25)\nwhile [ ! -f release ]; do sleep 0.05; done\n";
    let mut rig = start_rig(script).await.expect("start the progress rig");
    let call = tool_call_id(&rig.seen).await.expect("the probe runs");
    let (rows, marked) = progress_rows_until(&mut rig.subscription, &call, "L25")
        .await
        .expect("a progress row carries the full output");
    let expected = (7..=25)
        .map(|line| format!("L{line:02}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert_eq!(
        marked, expected,
        "the row carries exactly the last 20 lines"
    );
    assert_eq!(
        rows.iter().filter(|row| row.contains("L25")).count(),
        1,
        "the full snapshot publishes once, never as a duplicate"
    );
    std::fs::write(rig.workspace_dir.join("release"), b"").expect("release the child");
    assert!(
        !drain_until_end(&mut rig.subscription)
            .await
            .expect("the turn ends"),
        "the exit is not a cancellation"
    );
    let view = rig
        .agent
        .view(dal_core::PageReq::default())
        .expect("read the final view");
    assert!(
        format!("{view:?}").contains("probe exit Exited"),
        "the tool result reaches the journal after the exit"
    );
    assert_progress_quiet(&mut rig.subscription, &call)
        .await
        .expect("no progress follows the terminal outcome");
}

/// Progress observed before a cancel never resumes after the turn ends
/// cancelled.
#[tokio::test]
async fn progress_stops_at_cancellation() {
    let script = "echo READY\nsleep 30\n";
    let mut rig = start_rig(script).await.expect("start the progress rig");
    let call = tool_call_id(&rig.seen).await.expect("the probe runs");
    let (_, marked) = progress_rows_until(&mut rig.subscription, &call, "READY")
        .await
        .expect("a progress row carries the child output");
    assert!(
        marked.contains("READY"),
        "the pre-cancel row shows live output"
    );
    rig.agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(rig.turn),
        })
        .await
        .expect("the cancel is accepted");
    assert!(
        drain_until_end(&mut rig.subscription)
            .await
            .expect("the turn ends"),
        "the turn ends cancelled"
    );
    assert_progress_quiet(&mut rig.subscription, &call)
        .await
        .expect("no progress follows the cancellation");
}

/// A running child with periodic progress emits multiple distinct progress
/// updates through the real session broadcast subscription before exiting.
#[tokio::test]
async fn running_child_with_periodic_progress_emits_multiple_updates_through_session_broadcast() {
    let script = "printf 'PHASE_1\\n'\n\
                  while [ ! -f step2 ]; do sleep 0.05; done\n\
                  printf 'PHASE_2\\n'\n\
                  while [ ! -f release ]; do sleep 0.05; done\n";
    let mut rig = start_rig(script).await.expect("start the progress rig");
    let call = tool_call_id(&rig.seen).await.expect("the probe runs");

    // First progress update observed by the session subscriber.
    let (_, phase1) = progress_rows_until(&mut rig.subscription, &call, "PHASE_1")
        .await
        .expect("first periodic progress update arrives through session broadcast");
    assert!(
        phase1.contains("PHASE_1"),
        "the first update contains the first phase output"
    );
    assert!(!phase1.contains("PHASE_2"), "phase 2 has not started yet");

    // Signal the child to emit the next phase of output.
    std::fs::write(rig.workspace_dir.join("step2"), b"").expect("advance child to step2");

    // Second periodic progress update observed by the session subscriber.
    let (_, phase2) = progress_rows_until(&mut rig.subscription, &call, "PHASE_2")
        .await
        .expect("second periodic progress update arrives through session broadcast");
    assert!(
        phase2.contains("PHASE_1") && phase2.contains("PHASE_2"),
        "the second update contains cumulative tail lines through the session broadcast"
    );

    // Release the child to finish.
    std::fs::write(rig.workspace_dir.join("release"), b"").expect("release child to exit");
    assert!(
        !drain_until_end(&mut rig.subscription)
            .await
            .expect("the turn ends"),
        "the exit is not a cancellation"
    );
    assert_progress_quiet(&mut rig.subscription, &call)
        .await
        .expect("no progress follows the terminal outcome");
}
