#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! The scoped grant an approved tool call carries covers the `run` service
//! calls that the same extension makes for that call: while the call is in
//! flight and until the jobs the call started have ended. Nothing else
//! rides on it: not another call's work, not another argv, and not a
//! directory outside the approved roots or in another session's data.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, Caller, ExtensionBuilder, RawValue, Services, Tool, ToolCall, ToolCx,
    ToolOutcome, ToolOutput,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription, ToolError};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, ExitStatusKind, Expect, GrantSpec, JobId,
    JobOutcome, JobsOp, JobsReply, ModelInfo, Name, Part, Preview, RawJson, RunRequest, ServiceSet,
    ToolClass, ToolSpec, UpdateKind, Visibility, Workspace,
};
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(60);

/// Room for the commands and outcomes in flight; the test sends one at a time.
const COMMAND_QUEUE: usize = 16;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const USAGE: &str = "{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}";

const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// One provider step that calls the runner once per `modes` entry.
fn call_step(modes: &[&str]) -> String {
    let started = modes
        .iter()
        .map(|mode| {
            format!(
                "{{\"type\":\"tool_call_started\",\"id\":\"c-{mode}\",\"name\":\"fixture__runner\"}}"
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let calls = modes
        .iter()
        .map(|mode| {
            format!(
                "{{\"id\":\"c-{mode}\",\"name\":\"fixture__runner\",\"args\":{{\"kind\":\"parsed\",\"value\":{{\"mode\":\"{mode}\"}}}}}}"
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"kind\":\"events\",\"events\":[{started},{{\"type\":\"tool_calls_done\",\"calls\":[{calls}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    )
}

/// What the background worker of one call is told to do.
enum Cmd {
    /// Runs one argv through the `run` service as the call's extension.
    Run {
        label: &'static str,
        argv: &'static [&'static str],
        cwd: PathBuf,
    },
    /// Starts a child job under the job the call started.
    SpawnChild,
    /// Tries to start another top-level job after the call has ended.
    SpawnLateRoot,
    /// Settles the job the call started.
    Settle,
}

/// One `run` service outcome the fixture saw: `Ok` for a clean exit.
struct Seen {
    label: String,
    outcome: Result<(), String>,
}

/// State shared between the fixture tool and the test body.
struct Rig {
    workers: Mutex<HashMap<String, mpsc::Sender<Cmd>>>,
    history: Mutex<Vec<mpsc::Sender<Cmd>>>,
    tasks: Mutex<Vec<tokio_util::task::AbortOnDropHandle<()>>>,
    seen: mpsc::Sender<Seen>,
    data: PathBuf,
    dirs: Mutex<Option<(PathBuf, PathBuf)>>,
}

impl Rig {
    fn report(&self, label: &str, outcome: Result<(), String>) {
        self.seen
            .try_send(Seen {
                label: label.to_owned(),
                outcome,
            })
            .expect("the fixture outcome queue has room");
    }
}

fn request(argv: &[&str], cwd: &Path) -> RunRequest {
    RunRequest {
        argv: argv.iter().map(OsString::from).collect(),
        cwd: Some(cwd.to_path_buf()),
        stdin: None,
        timeout: Some(Duration::from_secs(20)),
        env: Vec::new(),
        stdout_prefix_limit: 4096,
    }
}

/// A model-visible tool in the shape of the orchestration battery. Mode
/// `approved` asks for approval with a `git` grant, makes one `run` call
/// inside the call, starts one job, and leaves a background worker that runs
/// whatever the test sends. Mode `plain` skips the approval, starts one job,
/// and leaves a worker.
struct Runner {
    name: Name,
    spec: Arc<ToolSpec>,
    rig: Arc<Rig>,
}

impl Tool for Runner {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, args: &RawValue, workspace: &Workspace) -> Result<ToolClass, ArgError> {
        if args.as_str().contains("\"plain\"") {
            return Ok(ToolClass::Other);
        }
        Ok(ToolClass::Exec {
            read_only: false,
            grant: Some(GrantSpec {
                argv_prefix: "git".into(),
                roots: vec![
                    workspace.as_path().to_path_buf(),
                    self.rig.data.join("worktrees"),
                ],
            }),
        })
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the fixture holds abort-on-drop ownership of each background worker"
    )]
    fn run<'a>(&'a self, call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let mode = if call.args.as_str().contains("\"plain\"") {
                "plain"
            } else {
                "approved"
            };
            let services = cx.services();
            let caller = cx.caller().clone();
            let workspace = cx.workspace().as_path().to_path_buf();
            if mode == "approved" {
                let preview = Preview {
                    title: "runner".into(),
                    body: "git under a grant".into(),
                    digest: None,
                };
                if let Err(reason) = cx.authorize(preview).await {
                    return ToolOutcome::Err(ToolError::Denied(reason));
                }
                let mine = self
                    .rig
                    .data
                    .join("worktrees")
                    .join(cx.session().to_string());
                let theirs = self.rig.data.join("worktrees").join("other-session");
                std::fs::create_dir_all(&mine).expect("own worktree dir");
                std::fs::create_dir_all(&theirs).expect("foreign worktree dir");
                *self.rig.dirs.lock().expect("dirs lock") = Some((mine, theirs));
                let preflight = services
                    .run(&caller, request(&["git", "--version"], &workspace))
                    .await;
                self.rig.report("preflight", exit_zero(preflight));
            }
            let name = Name::parse("grantrun").expect("job name");
            let payload = RawJson::parse("{}").expect("payload");
            let spawned = services
                .jobs(
                    &caller,
                    JobsOp::Spawn {
                        name,
                        payload,
                        parent: None,
                    },
                )
                .await;
            let Ok(JobsReply::Spawned { id }) = spawned else {
                return ToolOutcome::Err(ToolError::message("the job did not spawn"));
            };
            let (commands, inbox) = mpsc::channel::<Cmd>(COMMAND_QUEUE);
            self.rig
                .history
                .lock()
                .expect("history lock")
                .push(commands.clone());
            self.rig
                .workers
                .lock()
                .expect("workers lock")
                .insert(mode.to_owned(), commands);
            let worker = tokio::spawn(serve(self.rig.seen.clone(), services, caller, id, inbox));
            self.rig
                .tasks
                .lock()
                .expect("tasks lock")
                .push(tokio_util::task::AbortOnDropHandle::new(worker));
            ToolOutcome::Ok(ToolOutput::from_text("started"))
        })
    }
}
/// Runs the commands the test sends for one call's job, as that call's
/// extension, until the test drops the sender.
async fn serve(
    seen: mpsc::Sender<Seen>,
    services: Arc<dyn Services>,
    caller: Caller,
    id: JobId,
    mut inbox: mpsc::Receiver<Cmd>,
) {
    while let Some(cmd) = inbox.recv().await {
        let (label, outcome) = match cmd {
            Cmd::Run { label, argv, cwd } => {
                let ran = services.run(&caller, request(argv, &cwd)).await;
                (label, exit_zero(ran))
            }
            Cmd::SpawnChild => {
                let op = JobsOp::Spawn {
                    name: Name::parse("grantchild").expect("job name"),
                    payload: RawJson::parse("{}").expect("payload"),
                    parent: Some(id),
                };
                let spawned = services.jobs(&caller, op).await;
                ("spawned-child", job_reply(&spawned, "Spawned"))
            }
            Cmd::SpawnLateRoot => {
                let op = JobsOp::Spawn {
                    name: Name::parse("grantrun").expect("job name"),
                    payload: RawJson::parse("{}").expect("payload"),
                    parent: None,
                };
                let spawned = services.jobs(&caller, op).await;
                ("spawned-root", job_reply(&spawned, "Spawned"))
            }
            Cmd::Settle => {
                let op = JobsOp::Settle {
                    id,
                    outcome: JobOutcome::Exited { code: 0 },
                    text: "done".into(),
                };
                let settled = services.jobs(&caller, op).await;
                ("settled", job_reply(&settled, "Settled"))
            }
        };
        seen.try_send(Seen {
            label: label.to_owned(),
            outcome,
        })
        .expect("the fixture outcome queue has room");
    }
}

/// Folds one `jobs` service reply into pass/fail by its variant name.
fn job_reply(
    reply: &Result<JobsReply, dal_agent::ServiceError>,
    variant: &str,
) -> Result<(), String> {
    match reply {
        Ok(found) if format!("{found:?}").starts_with(variant) => Ok(()),
        other => Err(format!("{other:?}")),
    }
}

/// Folds one `run` service result into the fixture's pass/fail shape.
fn exit_zero(outcome: Result<dal_core::RunOutput, dal_agent::ServiceError>) -> Result<(), String> {
    match outcome {
        Ok(output) if output.status == ExitStatusKind::Exited(0) => Ok(()),
        Ok(output) => Err(format!("exited {:?}", output.status)),
        Err(error) => Err(format!("{error:?}")),
    }
}

struct Session {
    agent: Agent,
    subscription: Subscription,
    seen: mpsc::Receiver<Seen>,
    rig: Arc<Rig>,
    workspace: PathBuf,
    _host: Host,
    _tmp: tempfile::TempDir,
}

/// Starts one host with the runner registered and one scripted turn that
/// calls it once per mode.
async fn start_turn(modes: &[&str]) -> Result<Session, Box<dyn std::error::Error>> {
    start_configured(modes, 1, None).await
}

async fn start_configured(
    modes: &[&str],
    repeats: usize,
    workspace_subdir: Option<&str>,
) -> Result<Session, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir =
        workspace_subdir.map_or_else(|| tmp.path().join("w"), |subdir| data.join(subdir));
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let fixture = data.join("script.jsonl");
    std::fs::write(
        &fixture,
        format!("{}{STEP_END}", call_step(modes).repeat(repeats)),
    )?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str()))?;
    let (seen_tx, seen) = mpsc::channel(COMMAND_QUEUE);
    let rig = Arc::new(Rig {
        workers: Mutex::new(HashMap::new()),
        history: Mutex::new(Vec::new()),
        tasks: Mutex::new(Vec::new()),
        seen: seen_tx,
        data: data.clone(),
        dirs: Mutex::new(None),
    });
    let runner = Arc::new(Runner {
        name: Name::parse("fixture__runner")?,
        spec: Arc::new(ToolSpec {
            name: Name::parse("fixture__runner")?,
            description: "grant runner tool".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#)?,
            grammar: None,
        }),
        rig: Arc::clone(&rig),
    });
    let extension =
        ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::from_names(["jobs", "run"])?)?
            .tool(runner, Visibility::Model)
            .build()?;
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
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
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace_dir.clone())?,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await?;
    let subscription = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "run the runner".into(),
            }],
        })
        .await?;
    assert!(
        matches!(reply, dal_core::Reply::Accepted { .. }),
        "prompt not accepted: {reply:?}"
    );
    Ok(Session {
        agent,
        subscription,
        seen,
        rig,
        workspace: workspace_dir,
        _host: host,
        _tmp: tmp,
    })
}

impl Session {
    /// Answers every approval request with `answer` until the turn ends and
    /// returns how many requests opened.
    async fn finish_turn(&mut self, answer: &Answer) -> Result<usize, Box<dyn std::error::Error>> {
        let mut opened = 0;
        for _ in 0..400 {
            let delivery = tokio::time::timeout(WAIT, self.subscription.next())
                .await?
                .ok_or("the session closed before the turn ended")?;
            let Delivery::Update(update) = &delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => {
                    opened += 1;
                    self.agent.answer(request.id, answer.clone()).await?;
                }
                UpdateKind::TurnEnded { .. } => return Ok(opened),
                _ => {}
            }
        }
        Err("the turn never ended".into())
    }

    /// Takes the next fixture outcome.
    async fn next_seen(&mut self) -> Result<Seen, Box<dyn std::error::Error>> {
        Ok(tokio::time::timeout(WAIT, self.seen.recv())
            .await?
            .ok_or("the fixture stopped reporting")?)
    }

    /// Sends one command to the worker of the call in `mode` and returns the
    /// outcome it reports. A run call that no grant covers asks the front
    /// end; this declines every such question, so an uncovered call ends in
    /// an error and a covered call must succeed without one.
    async fn send(&mut self, mode: &str, cmd: Cmd) -> Result<Seen, Box<dyn std::error::Error>> {
        self.rig
            .workers
            .lock()
            .expect("workers lock")
            .get(mode)
            .ok_or("no worker for that call")?
            .try_send(cmd)
            .map_err(|error| format!("the worker did not take the command: {error}"))?;
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            tokio::select! {
                seen = self.seen.recv() => {
                    return Ok(seen.ok_or("the fixture stopped reporting")?);
                }
                delivery = self.subscription.next() => {
                    self.decline(delivery.ok_or("the session closed during a send")?).await?;
                }
                () = tokio::time::sleep_until(deadline) => {
                    return Err("the fixture reported no outcome".into());
                }
            }
        }
    }

    /// Declines the approval request that `delivery` opens, if it opens one.
    async fn decline(&self, delivery: Delivery) -> Result<(), Box<dyn std::error::Error>> {
        let Delivery::Update(update) = &delivery else {
            return Ok(());
        };
        let UpdateKind::RequestOpened(request) = &update.kind else {
            return Ok(());
        };
        self.agent.answer(request.id, Answer::Decline).await?;
        Ok(())
    }

    fn run_cmd(label: &'static str, argv: &'static [&'static str], cwd: &Path) -> Cmd {
        Cmd::Run {
            label,
            argv,
            cwd: cwd.to_path_buf(),
        }
    }
}

#[tokio::test]
async fn an_approved_call_covers_its_own_preflight_and_run_calls_until_its_job_ends() -> TestResult
{
    let mut session = start_turn(&["approved"]).await?;
    let opened = session.finish_turn(&Answer::Approve).await?;
    assert_eq!(opened, 1, "one approval covers the call and its run calls");

    let preflight = session.next_seen().await?;
    assert_eq!(preflight.label, "preflight");
    assert_eq!(
        preflight.outcome,
        Ok(()),
        "a run call inside the approved call needs no second approval"
    );

    let workspace = session.workspace.clone();
    let background = session
        .send(
            "approved",
            Session::run_cmd("background", &["git", "--version"], &workspace),
        )
        .await?;
    assert_eq!(
        background.outcome,
        Ok(()),
        "a run call from the approved call's job needs no second approval"
    );

    let other_argv = session
        .send(
            "approved",
            Session::run_cmd("other-argv", &["echo", "bg"], &workspace),
        )
        .await?;
    assert!(
        other_argv.outcome.is_err(),
        "an argv outside the grant prefix is not covered"
    );

    let outside = session.rig.data.parent().ok_or("no parent")?.to_path_buf();
    let escaped = session
        .send(
            "approved",
            Session::run_cmd("outside-roots", &["git", "--version"], &outside),
        )
        .await?;
    assert!(
        escaped.outcome.is_err(),
        "a directory outside the grant roots is not covered"
    );

    let settled = session.send("approved", Cmd::Settle).await?;
    assert_eq!(settled.label, "settled");
    let after = session
        .send(
            "approved",
            Session::run_cmd("after-end", &["git", "--version"], &workspace),
        )
        .await?;
    assert!(
        after.outcome.is_err(),
        "the grant ends with the job the call started"
    );
    Ok(())
}

#[tokio::test]
async fn a_call_without_its_own_approval_gets_nothing_from_a_sibling_calls_grant() -> TestResult {
    let mut session = start_turn(&["approved", "plain"]).await?;
    session.finish_turn(&Answer::Approve).await?;
    let preflight = session.next_seen().await?;
    assert_eq!(preflight.outcome, Ok(()));

    let workspace = session.workspace.clone();
    let sibling = session
        .send(
            "plain",
            Session::run_cmd("sibling", &["git", "--version"], &workspace),
        )
        .await?;
    assert!(
        sibling.outcome.is_err(),
        "a grant belongs to the call that earned it, not to its extension"
    );
    let own = session
        .send(
            "approved",
            Session::run_cmd("own", &["git", "--version"], &workspace),
        )
        .await?;
    assert_eq!(own.outcome, Ok(()));

    session.send("approved", Cmd::Settle).await?;
    let still = session
        .send(
            "plain",
            Session::run_cmd("sibling-after", &["git", "--version"], &workspace),
        )
        .await?;
    assert!(
        still.outcome.is_err(),
        "ending the approved call's job does not hand its grant to the sibling"
    );
    Ok(())
}

#[tokio::test]
async fn a_grant_root_in_the_data_root_covers_only_this_sessions_directory() -> TestResult {
    let mut session = start_turn(&["approved"]).await?;
    session.finish_turn(&Answer::Approve).await?;
    session.next_seen().await?;
    let (mine, theirs) = session
        .rig
        .dirs
        .lock()
        .expect("dirs lock")
        .clone()
        .ok_or("the fixture made no directories")?;

    let own = session
        .send(
            "approved",
            Session::run_cmd("own-session", &["git", "--version"], &mine),
        )
        .await?;
    assert_eq!(
        own.outcome,
        Ok(()),
        "this session's own directory is covered"
    );
    let foreign = session
        .send(
            "approved",
            Session::run_cmd("other-session", &["git", "--version"], &theirs),
        )
        .await?;
    assert!(
        foreign.outcome.is_err(),
        "another session's directory under the data root is not covered"
    );
    Ok(())
}

#[tokio::test]
async fn a_child_job_does_not_keep_the_grant_alive_after_its_run_job_ends() -> TestResult {
    let mut session = start_turn(&["approved"]).await?;
    session.finish_turn(&Answer::Approve).await?;
    session.next_seen().await?;

    let child = session.send("approved", Cmd::SpawnChild).await?;
    assert_eq!(child.outcome, Ok(()), "the child job starts");
    let workspace = session.workspace.clone();
    let while_live = session
        .send(
            "approved",
            Session::run_cmd("while-live", &["git", "--version"], &workspace),
        )
        .await?;
    assert_eq!(while_live.outcome, Ok(()));

    session.send("approved", Cmd::Settle).await?;
    let after = session
        .send(
            "approved",
            Session::run_cmd("child-only", &["git", "--version"], &workspace),
        )
        .await?;
    assert!(
        after.outcome.is_err(),
        "the grant ends with the run job, not with its last child"
    );
    Ok(())
}

#[tokio::test]
async fn repeated_provider_call_ids_do_not_replace_an_earlier_runs_grant() -> TestResult {
    let mut session = start_configured(&["approved"], 2, None).await?;
    assert_eq!(session.finish_turn(&Answer::Approve).await?, 2);
    assert_eq!(session.next_seen().await?.outcome, Ok(()));
    assert_eq!(session.next_seen().await?.outcome, Ok(()));
    let first = session
        .rig
        .history
        .lock()
        .expect("history")
        .first()
        .cloned()
        .ok_or("the first call did not start")?;
    session.send("approved", Cmd::Settle).await?;
    first.try_send(Session::run_cmd(
        "first-still-live",
        &["git", "--version"],
        &session.workspace,
    ))?;
    assert_eq!(
        session.next_seen().await?.outcome,
        Ok(()),
        "ending the second call's run must not revoke the first run's grant"
    );
    first.try_send(Cmd::Settle)?;
    session.next_seen().await?;
    Ok(())
}

#[tokio::test]
async fn a_late_top_level_job_cannot_revive_an_ended_calls_grant() -> TestResult {
    let mut session = start_turn(&["approved"]).await?;
    session.finish_turn(&Answer::Approve).await?;
    session.next_seen().await?;
    session.send("approved", Cmd::Settle).await?;
    assert_eq!(
        session.send("approved", Cmd::SpawnLateRoot).await?.outcome,
        Ok(())
    );
    let workspace = session.workspace.clone();
    let after = session
        .send(
            "approved",
            Session::run_cmd("after-late-root", &["git", "--version"], &workspace),
        )
        .await?;
    assert!(
        after.outcome.is_err(),
        "only the run minted while the call was live can keep its grant"
    );
    Ok(())
}

#[tokio::test]
async fn a_workspace_inside_the_data_root_is_not_narrowed_to_a_session() -> TestResult {
    let mut session = start_configured(&["approved"], 1, Some("checkout")).await?;
    session.finish_turn(&Answer::Approve).await?;
    let preflight = session.next_seen().await?;
    assert_eq!(
        preflight.outcome,
        Ok(()),
        "the checkout itself remains an approved working directory"
    );
    Ok(())
}

#[tokio::test]
async fn a_workspace_in_the_shared_worktree_root_does_not_grant_other_sessions() -> TestResult {
    let mut session = start_configured(&["approved"], 1, Some("worktrees/checkout")).await?;
    session.finish_turn(&Answer::Approve).await?;
    assert_eq!(session.next_seen().await?.outcome, Ok(()));
    let (_, theirs) = session
        .rig
        .dirs
        .lock()
        .expect("dirs")
        .clone()
        .ok_or("session worktree directories missing")?;
    let foreign = session
        .send(
            "approved",
            Session::run_cmd("foreign-sibling", &["git", "--version"], &theirs),
        )
        .await?;
    assert!(
        foreign.outcome.is_err(),
        "preserving the checkout must not preserve the whole shared worktree tree"
    );
    Ok(())
}
