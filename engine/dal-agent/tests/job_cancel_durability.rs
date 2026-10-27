//! Cancel durability: a cancel issued through the public `Agent::submit`
//! command path must not ack durable success unless its `End` line reaches
//! the job ledger. This test forces a real filesystem outage (a regular
//! file where the session `jobs/` directory belongs), cancels the job, and
//! requires the command to refuse with `AgentError::PersistenceFailed`. It
//! then closes the session through the owned `Host::close` while the
//! outage still holds, so the close-time flush also fails and cannot
//! persist the missing `End`. It restores the directory, reopens the same
//! durable session on a fresh host, and requires the public `Take` to
//! offer nothing for the cancelled job: acknowledgment versus disk must
//! agree, so no success ack may stand over a non-durable `End`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx, ToolOutcome,
    ToolOutput,
};
use dal_agent::{AgentError, Delivery, Env, Host, Product, SessionRef, Subscription, ToolError};
use dal_core::{
    Answer, CancelScope, ClientId, Command, Config, ConfigProduct, Expect, JobId, JobReport,
    JobsOp, JobsReply, ModelInfo, Name, Part, RawJson, ServiceSet, ToolClass, ToolSpec, UpdateKind,
    Visibility, Workspace,
};
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(60);

/// Room for the spawned id and the taken reports in flight.
const COMMAND_QUEUE: usize = 16;

const USAGE: &str = "{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}";

const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// One scripted provider step that calls the probe once in `mode`.
fn step(tool: &str, mode: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"c-1\",\"name\":\"{tool}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"c-1\",\"name\":\"{tool}\",\"args\":{{\"kind\":\"parsed\",\"value\":{{\"mode\":\"{mode}\"}}}}}}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    )
}

/// Minimal fixture tool: `spawn` mode admits one owner job and reports its
/// id; `take` mode takes ended top-level reports and reports them back.
struct Probe {
    name: Name,
    spec: Arc<ToolSpec>,
    spawned: mpsc::Sender<JobId>,
    taken: mpsc::Sender<Vec<JobReport>>,
}

impl Tool for Probe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Other)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let services = cx.services();
            let caller = cx.caller().clone();
            if call.args.as_str().contains("take") {
                match services.jobs(&caller, JobsOp::Take { limit: 10 }).await {
                    Ok(JobsReply::Taken(reports)) => {
                        let _ = self.taken.send(reports).await;
                        ToolOutcome::Ok(Box::new(ToolOutput::from_text("took")))
                    }
                    other => {
                        ToolOutcome::Err(ToolError::message(format!("take failed: {other:?}")))
                    }
                }
            } else {
                let name = match Name::parse("cancelprobe") {
                    Ok(name) => name,
                    Err(error) => {
                        return ToolOutcome::Err(ToolError::message(format!(
                            "bad job name: {error:?}"
                        )));
                    }
                };
                let payload = match RawJson::parse("{}") {
                    Ok(payload) => payload,
                    Err(error) => {
                        return ToolOutcome::Err(ToolError::message(format!(
                            "bad payload: {error:?}"
                        )));
                    }
                };
                match services
                    .jobs(
                        &caller,
                        JobsOp::Spawn {
                            name,
                            payload,
                            parent: None,
                        },
                    )
                    .await
                {
                    Ok(JobsReply::Spawned { id }) => {
                        let _ = self.spawned.send(id).await;
                        ToolOutcome::Ok(Box::new(ToolOutput::from_text("spawned")))
                    }
                    other => {
                        ToolOutcome::Err(ToolError::message(format!("spawn failed: {other:?}")))
                    }
                }
            }
        })
    }
}

/// Starts one host over the probe with a scripted turn calling it once in
/// `mode`. `resume` reopens an existing session; `None` creates a new one.
async fn start_host_with_probe(
    root: &Path,
    data: &Path,
    workspace: &Path,
    mode: &str,
    resume: Option<Box<str>>,
) -> Result<
    (
        Host,
        dal_agent::Agent,
        Subscription,
        mpsc::Receiver<JobId>,
        mpsc::Receiver<Vec<JobReport>>,
    ),
    Box<dyn std::error::Error>,
> {
    let script = data.join("script.jsonl");
    std::fs::create_dir_all(data)?;
    std::fs::create_dir_all(workspace)?;
    std::fs::write(
        &script,
        format!("{}{STEP_END}", step("fixture__probe", mode)),
    )?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        script.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, data, "", Some(user.as_str()))?;
    let (tx_spawned, rx_spawned) = mpsc::channel(COMMAND_QUEUE);
    let (tx_taken, rx_taken) = mpsc::channel(COMMAND_QUEUE);
    let probe = Arc::new(Probe {
        name: Name::parse("fixture__probe")?,
        spec: Arc::new(ToolSpec {
            name: Name::parse("fixture__probe")?,
            description: "cancel durability probe".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#)?,
            grammar: None,
        }),
        spawned: tx_spawned,
        taken: tx_taken,
    });
    let inject = ServiceSet::from_names(["jobs"])?;
    let extension = ExtensionBuilder::new("fixture", "0.1.0", inject)?
        .tool(probe, Visibility::Model)
        .build()?;
    let product = Product {
        name: "dal",
        data_root: data.to_path_buf(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (OsString::from("HOME"), OsString::from(root.join("home"))),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(root.join("cache")),
            ),
        ]),
        cwd: workspace.to_path_buf(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let reference = match resume {
        Some(key) => SessionRef::Resume {
            key,
            workspace: Workspace::new(workspace.to_path_buf())?,
        },
        None => SessionRef::New {
            workspace: Workspace::new(workspace.to_path_buf())?,
            name: None,
        },
    };
    let agent = host.open(reference, ClientId::new("probe")).await?;
    let subscription = agent.subscribe(None)?;
    Ok((host, agent, subscription, rx_spawned, rx_taken))
}

/// Answers every approval request until the turn ends.
async fn wait_turn_end(
    agent: &dal_agent::Agent,
    subscription: &mut Subscription,
) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..400 {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await?
            .ok_or("the session closed before the turn ended")?;
        let Delivery::Update(update) = &delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::RequestOpened(request) => {
                agent.answer(request.id, Answer::Approve).await?;
            }
            UpdateKind::TurnEnded { .. } => return Ok(()),
            _ => {}
        }
    }
    Err("the turn never ended".into())
}

/// Finds the one live session `jobs/` directory under the data root.
fn find_jobs_dir(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut stack = vec![root.to_path_buf()];
    let mut found = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if !path.is_dir() {
                continue;
            }
            if path.file_name().is_some_and(|name| name == "jobs") {
                found.push(path);
            } else {
                stack.push(path);
            }
        }
    }
    let [only]: [PathBuf; 1] = found.try_into().map_err(|found: Vec<PathBuf>| {
        format!("expected one session jobs dir, found {}", found.len())
    })?;
    Ok(only)
}

/// Replaces the session `jobs/` directory with a regular file, proving the
/// outage is a real filesystem failure. Returns the stashed directory.
fn block(dir: &Path) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    if !dir.is_dir() {
        return Err("the session jobs dir is not a directory".into());
    }
    let aside = dir.with_extension("aside");
    if aside.exists() {
        return Err("the aside path already exists".into());
    }
    std::fs::rename(dir, &aside)?;
    std::fs::write(dir, b"not a directory")?;
    if std::fs::write(dir.join("probe"), b"x").is_ok() {
        return Err("the blocked jobs path still accepts writes".into());
    }
    Ok(Some(aside))
}

/// Removes the blocking file and restores the stashed directory.
fn unblock(dir: &Path, aside: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    if dir.is_file() || dir.is_symlink() {
        std::fs::remove_file(dir)?;
    }
    if let Some(source) = aside
        && !dir.exists()
    {
        std::fs::rename(&source, dir)?;
    }
    if !dir.is_dir() {
        return Err("the jobs dir was not restored".into());
    }
    Ok(())
}

#[tokio::test]
async fn actor_cancel_ack_is_durable_across_a_blocked_ledger()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace = tmp.path().join("w");

    // Phase A: admit one owner job through the real tool service path.
    let (host_a, agent_a, mut sub_a, mut rx_spawned, _rx_taken_a) =
        start_host_with_probe(tmp.path(), &data, &workspace, "spawn", None).await?;
    let reply = agent_a
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "run the probe".into(),
            }],
        })
        .await?;
    assert!(
        matches!(&reply, dal_core::Reply::Accepted { .. }),
        "the prompt was not accepted: {reply:?}"
    );
    wait_turn_end(&agent_a, &mut sub_a).await?;
    let id: JobId = tokio::time::timeout(WAIT, rx_spawned.recv())
        .await?
        .ok_or("the probe never spawned its job")?;
    let key: Box<str> = agent_a.session().to_string().into();

    // Phase B: block the durable store for real, then cancel on the command
    // path. The cancel must not ack success its `End` cannot back.
    let jobs = find_jobs_dir(&data)?;
    let aside = block(&jobs)?;
    let first = agent_a
        .submit(Command::Cancel {
            scope: CancelScope::Job(id),
        })
        .await;

    // Observe the acknowledgment before shutdown: the blocked cancel must
    // surface as the command path's persistence failure, never ack durable
    // success.
    assert!(
        matches!(&first, Err(AgentError::PersistenceFailed { .. })),
        "the blocked cancel of {id} returned {first:?}, not PersistenceFailed"
    );

    // Phase C: close through the real owned teardown while the outage still
    // holds, and wait for it, so the session lock is deterministically
    // released before any reopen (bare drops never wait for the actor to
    // release it, and the next open fails `SessionBusy`). The close-time
    // flush runs against the blocked ledger and fails, so the close cannot
    // persist the missing `End` and hide the gap. Only then restore the
    // directory.
    host_a.close(agent_a.session()).await?;
    drop(sub_a);
    drop(agent_a);
    drop(host_a);
    unblock(&jobs, aside)?;

    // Phase D: reopen the same durable session on a fresh host and take
    // reports through the public path. The blocked cancel left no durable
    // `End` — the ledger persists only `End` lines, and neither the cancel
    // nor the blocked close flushed one — so the reopened table must not
    // offer the cancelled job: a success ack over this disk state is the
    // durability gap under attack.
    let (host_b, agent_b, mut sub_b, _rx_spawned_b, mut rx_taken) =
        start_host_with_probe(tmp.path(), &data, &workspace, "take", Some(key)).await?;
    let reply = agent_b
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "take the reports".into(),
            }],
        })
        .await?;
    assert!(
        matches!(&reply, dal_core::Reply::Accepted { .. }),
        "the take prompt was not accepted: {reply:?}"
    );
    wait_turn_end(&agent_b, &mut sub_b).await?;
    let reports = tokio::time::timeout(WAIT, rx_taken.recv())
        .await?
        .ok_or("the taker never reported")?;
    assert!(
        reports.iter().all(|report| report.id != id),
        "the reopened ledger offered the cancelled job {id} whose End never reached disk: {reports:?}"
    );
    host_b.close(agent_b.session()).await?;
    drop(sub_b);
    drop(agent_b);
    drop(host_b);

    Ok(())
}
