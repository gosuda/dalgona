// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The host-backed executor of `dalgona eval`: one started host per arm, one
//! ephemeral session per run.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;
use tokio::task::JoinSet;

use dal_agent::{Agent, Delivery, Env, Host, SessionRef, Subscription};
use dal_core::{
    ApprovalMode, CancelScope, ClientId, Command, Config, ConfigOverrides, ConfigProduct, Expect,
    Part, Reply, SessionId, Stop, StreamChannel, TurnId, UpdateKind, Workspace,
};

use dalgona_batteries::eval::CancellationToken;
use dalgona_batteries::eval::pack::{Exit, ToolCall, Usage};
use dalgona_batteries::eval::probe::{JudgeAccess, Probe, Transcript};
use dalgona_batteries::eval::run::{
    ExecError, ExecFuture, Executor, JudgeResult, Outcome, RunOutput, RunRequest,
};

/// The client id the runner opens its sessions as.
const CLIENT: &str = "dalgona-eval";

/// How long a started host may take to close its sessions.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// The text-and-config inputs of one arm.
pub(crate) struct ArmInputs<'a> {
    /// The product data root.
    pub data_root: &'a Path,
    /// The user `dal.toml` text, when the file exists.
    pub user_toml: Option<&'a str>,
    /// The `--against` file text layered over the user file.
    pub against_toml: Option<&'a str>,
    /// The `--model` override.
    pub model: Option<&'a str>,
}

fn arm_error(error: impl std::fmt::Display) -> ExecError {
    ExecError::Arm(error.to_string().into())
}

fn session_error(error: impl std::fmt::Display) -> ExecError {
    ExecError::Session(error.to_string().into())
}

/// Replaces `base` keys with `over` keys, merging tables by key path. Arrays
/// and scalars replace whole, as the product's own config layering does.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base)), toml::Value::Table(over)) => merge(base, over),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

fn parse_table(text: &str, what: &str) -> Result<toml::Table, ExecError> {
    text.parse::<toml::Table>()
        .map_err(|error| arm_error(format!("{what}: {error}")))
}

/// The user layer of one arm: the user file with the `--against` file on top.
fn layered(inputs: &ArmInputs<'_>) -> Result<Option<String>, ExecError> {
    let mut table = match inputs.user_toml {
        Some(text) => parse_table(text, "dal.toml")?,
        None => toml::Table::new(),
    };
    if let Some(text) = inputs.against_toml {
        merge(&mut table, parse_table(text, "--against")?);
    }
    if table.is_empty() {
        return Ok(None);
    }
    toml::to_string(&table).map(Some).map_err(arm_error)
}

/// Loads the configuration one arm runs under: the product defaults, the
/// layered user files, the model override, and approval `edits`: edits to
/// the run's own temporary workspace proceed, and every call that still needs
/// approval fails closed because a run has no controller to answer it.
///
/// # Errors
/// Returns [`ExecError::Arm`] with the config error text.
pub(crate) fn load_config(inputs: &ArmInputs<'_>) -> Result<Config, ExecError> {
    let user = layered(inputs)?;
    let factory = crate::product();
    let config = Config::load(
        ConfigProduct::Dalgona,
        inputs.data_root,
        factory.defaults,
        user.as_deref(),
    )
    .map_err(arm_error)?;
    Ok(config.with_overrides(&ConfigOverrides {
        model: inputs.model.map(Into::into),
        approval: Some(ApprovalMode::Edits),
        ..ConfigOverrides::default()
    }))
}

/// One arm's started host and its probe.
pub(crate) struct ArmHost {
    host: Host,
    probe: Arc<Probe>,
    /// The run tasks this host owns. `Executor::run` hands each session
    /// lifecycle to the set, so a dropped run future still closes its
    /// session instead of leaking it until host shutdown.
    runs: Mutex<JoinSet<()>>,
}

impl ArmHost {
    /// Builds the dalgona product over `config`, adds the probe, and starts a host.
    ///
    /// # Errors
    /// Returns [`ExecError::Arm`] when the product or the host cannot start.
    pub(crate) async fn start(
        config: Config,
        data_root: PathBuf,
        vars: BTreeMap<OsString, OsString>,
    ) -> Result<Arc<Self>, ExecError> {
        let mut product = crate::build(&dalgon::BuildCx {
            data_root,
            config: &config,
        })
        .map_err(arm_error)?;
        let probe = Probe::new();
        product
            .extensions
            .push(probe.extension().map_err(arm_error)?);
        let env = Env {
            vars,
            cwd: std::env::temp_dir(),
            sandbox_helper: None,
        };
        let host = Host::start(product, config, env).await.map_err(arm_error)?;
        Ok(Arc::new(Self {
            host,
            probe,
            runs: Mutex::new(JoinSet::new()),
        }))
    }

    /// Closes every session the host still holds.
    pub(crate) async fn shutdown(&self) {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .abort_all();
        self.host.clone().shutdown(SHUTDOWN_GRACE).await;
    }

    async fn run_session(
        host: &Host,
        probe: &Probe,
        request: RunRequest<'_>,
        cancel: CancellationToken,
    ) -> Result<Outcome, ExecError> {
        let workspace = Workspace::new(request.workspace.to_path_buf()).map_err(session_error)?;
        let agent = host
            .open(SessionRef::Ephemeral { workspace }, ClientId::new(CLIENT))
            .await
            .map_err(session_error)?;
        let session = agent.session();
        let outcome = Self::drive(probe, &agent, session, &request, &cancel).await;
        let _ = host.close(session).await;
        outcome
    }

    async fn drive(
        probe: &Probe,
        agent: &Agent,
        session: SessionId,
        request: &RunRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<Outcome, ExecError> {
        let judge = match request.judge {
            None => None,
            Some(_) => match probe.judge(session).await {
                JudgeAccess::Skip(skip) => return Ok(Outcome::JudgeSkipped(skip)),
                JudgeAccess::Ready(judge) => Some(judge),
            },
        };
        probe.allow_tools(session, request.tools);
        let mut subscription = agent.subscribe_listen(None).map_err(session_error)?;
        let started = Instant::now();
        let reply = agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: request.prompt.into(),
                }],
            })
            .await
            .map_err(session_error)?;
        let Reply::Accepted { turn, .. } = reply else {
            return Err(session_error("the session did not accept the prompt"));
        };
        let turn_record = collect(agent, &mut subscription, turn, cancel).await?;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let verdict = match (judge, request.judge) {
            (Some(judge), Some(question)) => {
                let transcript = Transcript {
                    prompt: request.prompt,
                    text: &turn_record.text,
                    tool_calls: &turn_record.tool_calls,
                };
                judge.ask(question, &transcript, turn).await
            }
            _ => JudgeResult::NotAsked,
        };
        Ok(Outcome::Ran(RunOutput {
            exit: turn_record.exit,
            text: turn_record.text,
            tool_calls: turn_record.tool_calls,
            usage: turn_record.usage,
            latency_ms,
            judge: verdict,
        }))
    }
}

/// A run request owned so the host's run task can outlive a dropped
/// executor future.
struct OwnedRunRequest {
    prompt: String,
    workspace: PathBuf,
    tools: Option<Vec<String>>,
    judge: Option<String>,
}

impl OwnedRunRequest {
    fn borrow(&self) -> RunRequest<'_> {
        RunRequest {
            prompt: &self.prompt,
            workspace: self.workspace.as_path(),
            tools: self.tools.as_deref(),
            judge: self.judge.as_deref(),
        }
    }
}

impl Executor for ArmHost {
    fn run<'a>(&'a self, request: RunRequest<'a>, cancel: CancellationToken) -> ExecFuture<'a> {
        let request = OwnedRunRequest {
            prompt: request.prompt.to_owned(),
            workspace: request.workspace.to_path_buf(),
            tools: request.tools.map(<[String]>::to_vec),
            judge: request.judge.map(Into::into),
        };
        let host = self.host.clone();
        let probe = Arc::clone(&self.probe);
        let (done, finished) = oneshot::channel();
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .spawn(async move {
                let outcome = Self::run_session(&host, &probe, request.borrow(), cancel).await;
                let _ = done.send(outcome);
            });
        Box::pin(async move { finished.await.unwrap_or(Err(ExecError::Interrupted)) })
    }
}

/// What one turn produced.
struct TurnRecord {
    exit: Exit,
    text: String,
    tool_calls: Vec<ToolCall>,
    usage: Usage,
}

fn exit_of(stop: Stop) -> Exit {
    match stop {
        Stop::EndTurn => Exit::Ok,
        Stop::Cancelled => Exit::Cancelled,
        _ => Exit::Failed,
    }
}

fn add_cost(total: Option<f64>, reported: Option<f64>) -> Option<f64> {
    match (total, reported) {
        (Some(total), Some(reported)) => Some(total + reported),
        (None, reported) => reported,
        (total, None) => total,
    }
}

/// Reads updates until `turn` ends, or until `cancel` fires and the turn is
/// cancelled.
async fn collect(
    agent: &Agent,
    subscription: &mut Subscription,
    turn: TurnId,
    cancel: &CancellationToken,
) -> Result<TurnRecord, ExecError> {
    let mut record = TurnRecord {
        exit: Exit::Failed,
        text: String::new(),
        tool_calls: Vec::new(),
        usage: Usage::default(),
    };
    loop {
        let delivery = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                let _ = agent
                    .submit(Command::Cancel { scope: CancelScope::Turn(turn) })
                    .await;
                return Err(ExecError::Interrupted);
            }
            delivery = subscription.next() => delivery,
        };
        let Some(Delivery::Update(update)) = delivery else {
            return Err(session_error("the session closed before the turn ended"));
        };
        match &update.kind {
            UpdateKind::Delta {
                turn: from,
                channel: StreamChannel::Text,
                text,
            } if *from == turn => record.text.push_str(text),
            UpdateKind::ToolStarted { tool, args, .. } => record.tool_calls.push(ToolCall {
                name: tool.to_string(),
                args: args.as_str().to_owned(),
            }),
            UpdateKind::Usage(view) => {
                record.usage.input_tokens += view.usage.input_tokens;
                record.usage.output_tokens += view.usage.output_tokens;
                record.usage.cost_usd = add_cost(record.usage.cost_usd, view.usage.cost_usd);
            }
            UpdateKind::TurnEnded { turn: ended, stop } if *ended == turn => {
                record.exit = exit_of(*stop);
                return Ok(record);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
