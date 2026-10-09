#![expect(clippy::expect_used, reason = "SC test")]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test writes private scripted fixtures"
)]
#![expect(
    dead_code,
    reason = "gate support exposes helpers shared across independent targets"
)]

//! Call grants stay bound to the approved argv, roots, and detached job.

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsString,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use dal_agent::{
    Delivery, Env, ProcStatus, SessionRef, StopReason, ToolError,
    ext::{
        ArgError, BoxFuture, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx, ToolOutcome,
        ToolOutput,
    },
};
use dal_core::{
    Answer, CallGrant, Command, DenyReason, Expect, GrantSpec, ModelInfo, Name, Origin, Part,
    Preview, RawJson, Reply, ToolClass, ToolSpec, Visibility, Workspace,
};
use support::{GateHarness, TestDir, scripted_session};

const TOOL_NAME: &str = "grant-run-probe";
const CHILD_MARKER_ENV: &str = "DAL_GATE_GRANT_CHILD_MARKER";

#[derive(Clone, Debug, Eq, PartialEq)]
enum CallResult {
    Detached,
    InRootExited,
    ArgvDenied(DenyReason),
    OutsideDenied(DenyReason),
    Revoked(DenyReason),
    Failed(String),
}

#[derive(Default)]
struct RunState(Mutex<Vec<CallResult>>);

impl RunState {
    fn push(&self, value: CallResult) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(value);
    }

    fn take(&self) -> Vec<CallResult> {
        std::mem::take(
            &mut *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

struct GrantTool {
    name: Name,
    spec: Arc<ToolSpec>,
    executable: PathBuf,
    workspace: PathBuf,
    outside: PathBuf,
    marker: PathBuf,
    next: AtomicUsize,
    state: Arc<RunState>,
}

impl GrantTool {
    fn preview() -> Preview {
        Preview {
            title: "Scoped process grant".into(),
            body: "run the test helper".into(),
            digest: Some([37; 32]),
        }
    }

    fn argv(&self, long_running: bool) -> Vec<OsString> {
        let mut argv = vec![self.executable.as_os_str().to_owned()];
        if long_running {
            argv.extend([
                OsString::from("--exact"),
                OsString::from("call_grant_child_wait"),
                OsString::from("--nocapture"),
            ]);
        } else {
            argv.push(OsString::from("--list"));
        }
        argv
    }

    fn options(cwd: PathBuf) -> dal_agent::SpawnOpts {
        dal_agent::SpawnOpts {
            cwd,
            timeout: None,
            env: Vec::new(),
            stdout_prefix_limit: 0,
        }
    }

    async fn authorize(&self, cx: &mut ToolCx<'_>) -> Result<dal_agent::ext::Approved, DenyReason> {
        cx.authorize(Self::preview()).await
    }

    async fn wait_for_marker(&self) -> Result<(), std::io::Error> {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self.marker.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(std::io::Error::other)?;
        tokio::time::sleep(Duration::from_millis(750)).await;
        Ok(())
    }
}

impl Tool for GrantTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Exec {
            read_only: false,
            grant: Some(GrantSpec {
                argv_prefix: self.executable.to_string_lossy().into_owned().into(),
                roots: vec![self.workspace.clone()],
            }),
        })
    }

    fn run<'a>(&'a self, _call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        let step = self.next.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if step == 0 {
                return self.detach_initial(cx).await;
            }
            if step == 1 {
                return self.run_in_root(&mut cx).await;
            }
            if step == 2 {
                return self.deny_argv_prefix(&mut cx).await;
            }
            if step == 3 {
                return self.deny_outside_root(&mut cx).await;
            }
            self.check_revoked(&mut cx).await
        })
    }
}

impl GrantTool {
    async fn detach_initial<'a>(&'a self, mut cx: ToolCx<'a>) -> ToolOutcome {
        let approved = match self.authorize(&mut cx).await {
            Ok(approved) => approved,
            Err(reason) => {
                self.state.push(CallResult::Failed(format!(
                    "initial authorization: {reason:?}"
                )));
                return ToolOutcome::Err(ToolError::Denied(reason));
            }
        };
        let proc = match cx.spawn(
            &self.argv(true),
            Self::options(self.workspace.clone()),
            approved,
        ) {
            Ok(proc) => proc,
            Err(error) => {
                self.state
                    .push(CallResult::Failed(format!("initial spawn: {error}")));
                return ToolOutcome::Err(error);
            }
        };
        let job = cx.detach(proc);
        self.state.push(CallResult::Detached);
        tokio::time::sleep(Duration::from_millis(200)).await;
        ToolOutcome::Detached(job)
    }

    async fn run_in_root(&self, cx: &mut ToolCx<'_>) -> ToolOutcome {
        let approved = match self.authorize(cx).await {
            Ok(approved) => approved,
            Err(reason) => {
                self.state.push(CallResult::Failed(format!(
                    "in-root authorization: {reason:?}"
                )));
                return ToolOutcome::Err(ToolError::Denied(reason));
            }
        };
        let mut proc = match cx.spawn(
            &self.argv(false),
            Self::options(self.workspace.clone()),
            approved,
        ) {
            Ok(proc) => proc,
            Err(error) => {
                self.state
                    .push(CallResult::Failed(format!("in-root spawn: {error}")));
                return ToolOutcome::Err(error);
            }
        };
        match proc.wait(cx.cancel()).await {
            Ok(result) if matches!(result.status, ProcStatus::Exited { code: 0 }) => {
                self.state.push(CallResult::InRootExited);
                ToolOutcome::Ok(Box::new(ToolOutput::from_text("in-root process completed")))
            }
            Ok(result) => {
                let message = format!("in-root process exited with {:?}", result.status);
                self.state.push(CallResult::Failed(message.clone()));
                ToolOutcome::Err(ToolError::message(message))
            }
            Err(error) => {
                self.state
                    .push(CallResult::Failed(format!("in-root wait: {error}")));
                ToolOutcome::Err(error)
            }
        }
    }

    async fn deny_argv_prefix(&self, cx: &mut ToolCx<'_>) -> ToolOutcome {
        let approved = match self.authorize(cx).await {
            Ok(approved) => approved,
            Err(reason) => {
                self.state.push(CallResult::Failed(format!(
                    "argv authorization: {reason:?}"
                )));
                return ToolOutcome::Err(ToolError::Denied(reason));
            }
        };
        let argv = [
            OsString::from("unapproved-command"),
            OsString::from("--list"),
        ];
        match cx.spawn(&argv, Self::options(self.workspace.clone()), approved) {
            Err(ToolError::Denied(reason @ DenyReason::OutOfScope { .. })) => {
                self.state.push(CallResult::ArgvDenied(reason.clone()));
                ToolOutcome::Err(ToolError::Denied(reason))
            }
            Err(error) => {
                self.state
                    .push(CallResult::Failed(format!("argv spawn: {error}")));
                ToolOutcome::Err(error)
            }
            Ok(mut proc) => {
                let _ = proc.stop(StopReason::Cancelled).await;
                self.state
                    .push(CallResult::Failed("unapproved argv prefix started".into()));
                ToolOutcome::Err(ToolError::message("unapproved argv prefix started"))
            }
        }
    }

    async fn deny_outside_root(&self, cx: &mut ToolCx<'_>) -> ToolOutcome {
        let approved = match self.authorize(cx).await {
            Ok(approved) => approved,
            Err(reason) => {
                self.state.push(CallResult::Failed(format!(
                    "outside authorization: {reason:?}"
                )));
                return ToolOutcome::Err(ToolError::Denied(reason));
            }
        };
        match cx.spawn(
            &self.argv(false),
            Self::options(self.outside.clone()),
            approved,
        ) {
            Err(ToolError::Denied(reason @ DenyReason::OutOfScope { .. })) => {
                self.state.push(CallResult::OutsideDenied(reason.clone()));
                if let Err(error) = self.wait_for_marker().await {
                    self.state.push(CallResult::Failed(format!(
                        "detached child did not finish: {error}"
                    )));
                }
                ToolOutcome::Err(ToolError::Denied(reason))
            }
            Err(error) => {
                self.state
                    .push(CallResult::Failed(format!("outside spawn: {error}")));
                ToolOutcome::Err(error)
            }
            Ok(mut proc) => {
                let _ = proc.stop(StopReason::Cancelled).await;
                self.state
                    .push(CallResult::Failed("outside-root process started".into()));
                ToolOutcome::Err(ToolError::message("outside-root process started"))
            }
        }
    }

    async fn check_revoked(&self, cx: &mut ToolCx<'_>) -> ToolOutcome {
        match self.authorize(cx).await {
            Err(reason @ DenyReason::NotGranted) => {
                self.state.push(CallResult::Revoked(reason.clone()));
                ToolOutcome::Err(ToolError::Denied(reason))
            }
            Err(reason) => {
                self.state.push(CallResult::Failed(format!(
                    "revocation returned {reason:?}"
                )));
                ToolOutcome::Err(ToolError::Denied(reason))
            }
            Ok(approved) => {
                let proc = match cx.spawn(
                    &self.argv(false),
                    Self::options(self.workspace.clone()),
                    approved,
                ) {
                    Ok(proc) => proc,
                    Err(error) => {
                        self.state
                            .push(CallResult::Failed(format!("revoked spawn: {error}")));
                        return ToolOutcome::Err(error);
                    }
                };
                let mut proc = proc;
                let _ = proc.stop(StopReason::Cancelled).await;
                self.state.push(CallResult::Failed(
                    "job grant remained live after child completion".into(),
                ));
                ToolOutcome::Err(ToolError::message(
                    "job grant remained live after child completion",
                ))
            }
        }
    }
}

struct ApprovalObservation {
    grant: Option<CallGrant>,
}

struct RunningHarness {
    _data: TestDir,
    workspace: TestDir,
    harness: GateHarness,
    state: Arc<RunState>,
}

fn tool_call_response(id: &str) -> String {
    let value = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "tool_call_started", "id": id, "name": TOOL_NAME},
            {"type": "tool_calls_done", "calls": [{
                "id": id,
                "name": TOOL_NAME,
                "args": {"kind": "parsed", "value": {}}
            }]},
            {"type": "usage", "usage": {
                "input_tokens": 1,
                "cached_input_tokens": 0,
                "output_tokens": 1,
                "reasoning_tokens": null,
                "cache_write_tokens": 0,
                "cost_usd": null
            }},
            {"type": "stop", "reason": "tool_use"}
        ]
    });
    sonic_rs::to_string(&value).expect("scripted tool fixture encodes as JSON")
}

fn scripted_text() -> String {
    r#"{"kind":"events","events":[{"type":"text_delta","text":"grant check complete"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#.into()
}

async fn start_session(
    workspace: TestDir,
    data: TestDir,
    marker: PathBuf,
) -> Result<RunningHarness, Box<dyn Error + Send + Sync>> {
    let executable = std::env::current_exe()?;
    let name = Name::parse(TOOL_NAME)?;
    let state = Arc::new(RunState::default());
    let spec = Arc::new(ToolSpec {
        name: name.clone(),
        description: "Exercise a job-scoped argv grant.".into(),
        parameters: RawJson::parse(r#"{"type":"object","additionalProperties":false}"#)?,
        grammar: None,
    });
    let tool = Arc::new(GrantTool {
        name,
        spec,
        executable,
        workspace: workspace.path().to_path_buf(),
        outside: data.path().join("outside"),
        marker: marker.clone(),
        next: AtomicUsize::new(0),
        state: Arc::clone(&state),
    });
    let extension = ExtensionBuilder::new("gate-call-grant", "0.1.0", dal_core::ServiceSet::EMPTY)?
        .with_origin(Origin::User, None)
        .tool(tool, Visibility::Model)
        .build()?;
    let fixture = data.path().join("call-grant.jsonl");
    fs::write(
        &fixture,
        [
            tool_call_response("grant-step-one"),
            tool_call_response("grant-step-two"),
            tool_call_response("grant-step-three"),
            tool_call_response("grant-step-four"),
            tool_call_response("grant-step-five"),
            scripted_text(),
        ]
        .join("\n"),
    )?;
    let factory = dalgon::product();
    let user = format!(
        "approval = \"ask\"\nmodel = \"openai-responses/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let mut product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    product.extensions.push(extension);
    let env = Env {
        vars: BTreeMap::from([(
            OsString::from(CHILD_MARKER_ENV),
            marker.as_os_str().to_owned(),
        )]),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::New {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
        name: None,
    };
    let harness = scripted_session(product, config, env, session).await?;
    Ok(RunningHarness {
        _data: data,
        workspace,
        harness,
        state,
    })
}

async fn drive_tool_sequence(
    harness: &GateHarness,
) -> Result<Vec<ApprovalObservation>, Box<dyn Error + Send + Sync>> {
    let mut subscription = harness.agent.subscribe(None)?;
    let reply = tokio::time::timeout(
        Duration::from_secs(30),
        harness.agent.submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Exercise the scoped run grant.".into(),
            }],
        }),
    )
    .await??;
    assert!(matches!(reply, Reply::Accepted { .. }));
    let mut approvals = Vec::new();
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(30), subscription.next())
            .await
            .map_err(|_| {
                let state = harness
                    .agent
                    .view(dal_core::PageReq::default())
                    .map_or_else(
                        |error| format!("view unavailable: {error}"),
                        |view| {
                            format!(
                                "turn={:?}, open={}, approvals_seen={}, last_entry={:?}",
                                view.turn,
                                view.open.len(),
                                approvals.len(),
                                view.entries.items.last().map(|entry| &entry.kind)
                            )
                        },
                    );
                std::io::Error::other(format!(
                    "scoped run grant turn timed out waiting for an update: {state}"
                ))
            })?
            .ok_or_else(|| std::io::Error::other("session update stream ended before turn end"))?;
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            dal_core::UpdateKind::RequestOpened(request) => match &request.question {
                dal_core::Question::Approval { grant, .. } => {
                    approvals.push(ApprovalObservation {
                        grant: grant.clone(),
                    });
                    let answer = if approvals.len() == 1 {
                        Answer::Approve
                    } else {
                        Answer::Decline
                    };
                    harness.agent.answer(request.id, answer).await?;
                }
                _ => harness.agent.answer(request.id, Answer::Decline).await?,
            },
            dal_core::UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    Ok(approvals)
}

#[test]
fn call_grant_child_wait() -> Result<(), Box<dyn Error>> {
    let Some(marker) = std::env::var_os(CHILD_MARKER_ENV) else {
        return Ok(());
    };
    std::thread::sleep(Duration::from_secs(2));
    fs::write(PathBuf::from(marker), b"finished")?;
    Ok(())
}

#[tokio::test]
async fn call_grant_is_limited_to_argv_roots_and_job() -> Result<(), Box<dyn Error + Send + Sync>> {
    let workspace = TestDir::new()?;
    let data = TestDir::new()?;
    fs::create_dir(data.path().join("outside"))?;
    let marker = workspace.path().join("grant-job-finished");
    let harness = start_session(workspace, data, marker.clone()).await?;
    let approvals = drive_tool_sequence(&harness.harness).await?;
    let results = harness.state.take();
    assert_eq!(approvals.len(), 1, "live-grant calls must not ask again");
    let grant = approvals
        .first()
        .and_then(|approval| approval.grant.as_ref())
        .expect("the approval carries a CallGrant");
    assert_eq!(
        grant.argv_prefix.as_ref(),
        std::env::current_exe()?.to_string_lossy().as_ref()
    );
    assert_eq!(grant.roots, [harness.workspace.path().to_path_buf()]);
    assert_eq!(results.len(), 5);
    assert_eq!(results.first().unwrap(), &CallResult::Detached);
    assert_eq!(results[1], CallResult::InRootExited);
    assert!(matches!(
        results[2],
        CallResult::ArgvDenied(DenyReason::OutOfScope { .. })
    ));
    assert!(matches!(
        results[3],
        CallResult::OutsideDenied(DenyReason::OutOfScope { .. })
    ));
    assert_eq!(results[4], CallResult::Revoked(DenyReason::NotGranted));
    assert!(
        marker.exists(),
        "the detached job did not write its completion marker"
    );
    let _ = harness.harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
