// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Disposable final-verification driver for the procs battery.
//!
//! Removed after the verification run. It exercises the real product build,
//! the registered procs tool object, the served manual pages, and a real
//! host start with a clean shutdown.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::Delivery;
use dal_agent::error::ServiceError;
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{
    Caller, EventStream, RawValue, Services, Tool, ToolCall, ToolCx, ToolOutcome,
};
use dal_core::ext::{
    JobsOp, JobsReply, McpDeclaration, McpRequest, McpResponse, PrivateEffect, StateError, StateOp,
    StateRecord,
};
use dal_core::{
    AgentsOp, AgentsReply, Answer, ConfigProduct, EntryId, Expect, FetchRequest, FetchResponse,
    Inference, ModelRequest, ModelRoute, Notice, Origin, Part, Question, RawJson, Reply, RunOutput,
    RunRequest, SidecarOp, Stop, TurnOp, TurnOpReply, UpdateKind, Visibility,
};

const EXPECTED_EMPTY: &str = "procs: no jobs in this session.";

const OTHER_BATTERIES: [&str; 11] = [
    "ask",
    "history",
    "judged",
    "mcp",
    "orchestration",
    "quality",
    "review",
    "skills",
    "ttsr-rules",
    "web",
    "work",
];

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct SmokeError {
    message: String,
}

fn fail(message: impl Into<String>) -> SmokeError {
    SmokeError {
        message: message.into(),
    }
}

/// Services seen by the registered procs tool: jobs only.
///
/// `None` answers an empty list (the isolated unit check). `Some` forwards the two
/// read-only operations `procs` performs (`List`, `Text`) to the Host-minted jobs
/// service under the Host-minted caller of one real session (the proof extension's
/// own `jobs` authority, builtin origin); every other operation is unavailable.
/// `Services::call_tool` cannot serve this: it ignores its caller and runs the tool
/// under a turnless builtin caller with an empty inject set, so the jobs gate
/// answers `NotInjected`.
struct JobsServices(Option<BoundServices>);

fn unavailable<T: Send + 'static>() -> ServiceFuture<'static, T> {
    Box::pin(async { Err(ServiceError::failed(None, "unused in procs smoke")) })
}

impl Services for JobsServices {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }
    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        unavailable()
    }
    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        unavailable()
    }
    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        unavailable()
    }
    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        unavailable()
    }
    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        unavailable()
    }
    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        unavailable()
    }
    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        unavailable()
    }
    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        unavailable()
    }
    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        unavailable()
    }
    fn private_outcomes(&self, _who: &Caller) -> ServiceFuture<'_, Vec<PrivateEffect>> {
        unavailable()
    }
    fn jobs(&self, _who: &Caller, op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        match (&self.0, op) {
            (Some((services, caller)), op @ (JobsOp::List | JobsOp::Text { .. })) => {
                services.jobs(caller, op)
            }
            (None, JobsOp::List) => Box::pin(async { Ok(JobsReply::Listed(Vec::new())) }),
            _ => unavailable(),
        }
    }
    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        unavailable()
    }
    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<dal_agent::ext::Doc>> {
        Box::pin(async { Ok(None) })
    }
    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        unavailable()
    }
    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }
    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        unavailable()
    }
    fn model_ready(
        &self,
        _who: &Caller,
        _model: ModelRoute,
    ) -> ServiceFuture<'_, Option<ModelRoute>> {
        unavailable()
    }
    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        unavailable()
    }
    fn infer_stream(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        unavailable()
    }
    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        unavailable()
    }
    fn notify(&self, _who: &Caller, _notice: Notice) {}
    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        unavailable()
    }
    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        unavailable()
    }
    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }
}

fn build_product(root: PathBuf, user: Option<&str>) -> Result<dal_agent::Product, SmokeError> {
    let factory = dalgona::product();
    let config = dal_core::Config::load(ConfigProduct::Dalgona, &root, factory.defaults, user)
        .map_err(|error| fail(format!("config load: {error:?}")))?;
    let cx = dalgon::BuildCx {
        data_root: root,
        config: &config,
    };
    dalgona::build(&cx).map_err(|error| fail(format!("product build: {error:?}")))
}

fn manual_page(name: &str) -> Result<String, SmokeError> {
    let manuals = (dalgona::product().docs)();
    let manual = manuals
        .iter()
        .find(|manual| manual.scheme == "dalgona")
        .ok_or_else(|| fail("no dalgona manual"))?;
    manual
        .pages
        .iter()
        .find(|entry| entry.0 == *name)
        .map(|entry| entry.1.clone())
        .ok_or_else(|| fail(format!("no dalgona://{name} page")))
}

fn check_default(product: &dal_agent::Product) -> Result<(), SmokeError> {
    let procs = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "procs")
        .ok_or_else(|| fail("default build registers no procs extension"))?;
    if procs.origin() != Origin::Bundled {
        return Err(fail(format!(
            "procs origin is {:?}, want Bundled",
            procs.origin()
        )));
    }
    check_others_present(product, "default")?;
    println!("procs-smoke default-on: PASS procs bundled, other 11 present");
    check_deferred(procs)?;
    check_manual()?;
    Ok(())
}

fn check_others_present(product: &dal_agent::Product, label: &str) -> Result<(), SmokeError> {
    for name in OTHER_BATTERIES {
        let present = product
            .extensions
            .iter()
            .any(|extension| extension.name() == name);
        if !present {
            return Err(fail(format!("{label} build omits battery {name}")));
        }
    }
    Ok(())
}

fn check_deferred(procs: &dal_agent::ext::Extension) -> Result<(), SmokeError> {
    if procs.tools().len() != 1 {
        return Err(fail(format!(
            "procs registers {} tools, want 1",
            procs.tools().len()
        )));
    }
    let (tool, visibility) = &procs.tools()[0];
    if tool.name().as_str() != "procs" {
        return Err(fail(format!(
            "procs tool is named {}",
            tool.name().as_str()
        )));
    }
    if *visibility != Visibility::Deferred {
        return Err(fail(format!(
            "procs visibility is {visibility:?}, want Deferred"
        )));
    }
    if !dalgona_batteries::procs::PROCS_DESCRIPTION.contains("background jobs") {
        return Err(fail("procs description names no background jobs"));
    }
    println!("procs-smoke deferred-discovery: PASS one Deferred tool named procs");
    Ok(())
}

fn check_manual() -> Result<(), SmokeError> {
    let served = manual_page("procs")?;
    if served != dalgona_batteries::procs::PROCS_DOC {
        return Err(fail("dalgona://procs body differs from PROCS_DOC"));
    }
    let symbols = manual_page("symbols")?;
    if !symbols.contains("dal://symbols") || !symbols.contains("dalgon docs dal://symbols") {
        return Err(fail("dalgona://symbols names no dal://symbols pointer"));
    }
    println!("procs-smoke manual-pages: PASS procs served, symbols pointer intact");
    Ok(())
}

async fn check_empty_list(tool: &Arc<dyn Tool>) -> Result<(), SmokeError> {
    let services: Arc<dyn Services> = Arc::new(JobsServices(None));
    let args =
        RawJson::parse(r#"{"op":"list"}"#).map_err(|error| fail(format!("args: {error:?}")))?;
    let outcome = tool
        .run(ToolCall::new("call", args), ToolCx::for_test(services))
        .await;
    let text = match outcome {
        ToolOutcome::Ok(output) => output.to_string(),
        ToolOutcome::Err(error) => error.to_string(),
        ToolOutcome::Interrupted => return Err(fail("procs call interrupted")),
        ToolOutcome::Detached(_) => return Err(fail("procs call detached")),
    };
    if text != EXPECTED_EMPTY {
        return Err(fail(format!(
            "empty list answered {text:?}, want {EXPECTED_EMPTY:?}"
        )));
    }
    println!("procs-smoke empty-job-list: PASS exact no-jobs text");
    Ok(())
}

fn check_disabled(base: &Path) -> Result<(), SmokeError> {
    let reduced = build_product(
        base.join("disabled"),
        Some("[plugin.procs]\nenabled = false\n"),
    )?;
    if reduced
        .extensions
        .iter()
        .any(|extension| extension.name() == "procs")
    {
        return Err(fail("disabled build still registers procs"));
    }
    check_others_present(&reduced, "disabled")?;
    println!("procs-smoke disabled-config: PASS procs absent, other 11 present");
    Ok(())
}

// Host mints both service handles and callers; no scripted Jobs service is used here.
type BoundServices = (Arc<dyn Services>, Caller);
struct Capture(tokio::sync::mpsc::Sender<BoundServices>);
impl dal_agent::ext::CommandHandler for Capture {
    fn run<'a>(
        &'a self,
        _args: &'a str,
        cx: dal_agent::ext::CommandCx<'a>,
    ) -> dal_agent::ext::BoxFuture<'a, Result<dal_core::Reply, ServiceError>> {
        Box::pin(async move {
            self.0
                .try_send((Arc::clone(cx.services()), cx.caller().clone()))
                .map_err(|error| ServiceError::failed(None, error.to_string()))?;
            Ok(dal_core::Reply::Done(dal_core::Output::Text(
                "captured".into(),
            )))
        })
    }
}

/// The registered procs tool's answer over one session's real Host jobs service.
///
/// The tool reports its no-jobs answer as a tool error (pinned by
/// `check_empty_list`), so exactly that text is an answer; any other error fails.
async fn procs_text(
    tool: &Arc<dyn Tool>,
    bound: &BoundServices,
    args: &str,
) -> Result<String, SmokeError> {
    let args = RawJson::parse(args).map_err(|error| fail(format!("procs args: {error:?}")))?;
    let services: Arc<dyn Services> =
        Arc::new(JobsServices(Some((Arc::clone(&bound.0), bound.1.clone()))));
    match tool
        .run(
            ToolCall::new("procs-proof-read", args),
            ToolCx::for_test(services),
        )
        .await
    {
        ToolOutcome::Ok(output) => Ok(output.to_string()),
        ToolOutcome::Err(error) => {
            let text = error.to_string();
            if text == EXPECTED_EMPTY {
                Ok(text)
            } else {
                Err(fail(format!("registered procs: {text}")))
            }
        }
        _ => Err(fail("registered procs did not finish inline")),
    }
}

struct HostChildTool {
    name: dal_core::Name,
    spec: Arc<dal_core::ToolSpec>,
}

impl HostChildTool {
    fn new(name: dal_core::Name) -> Result<Self, SmokeError> {
        const SCHEMA: &str = r#"{"type":"object","properties":{"code":{"type":"integer","enum":[0,7]}},"required":["code"],"additionalProperties":false}"#;
        let parameters =
            RawJson::parse(SCHEMA).map_err(|error| fail(format!("child schema: {error:?}")))?;
        let spec = Arc::new(dal_core::ToolSpec {
            name: name.clone(),
            description: "Owned bounded Host process proof".into(),
            parameters,
            grammar: None,
        });
        Ok(Self { name, spec })
    }
}

impl Tool for HostChildTool {
    fn name(&self) -> &dal_core::Name {
        &self.name
    }
    fn spec(&self, _model: &dal_core::ModelInfo) -> Arc<dal_core::ToolSpec> {
        Arc::clone(&self.spec)
    }
    fn classify(
        &self,
        _args: &RawValue,
        _ws: &dal_core::Workspace,
    ) -> Result<dal_core::ToolClass, dal_agent::ext::ArgError> {
        Ok(dal_core::ToolClass::Exec {
            read_only: false,
            grant: None,
        })
    }
    fn run<'a>(
        &'a self,
        call: ToolCall,
        mut cx: ToolCx<'a>,
    ) -> dal_agent::ext::BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let fields: BTreeMap<String, i32> = match call.args.decode_as() {
                Ok(fields) => fields,
                Err(_) => return ToolOutcome::Interrupted,
            };
            let Some(code @ (0 | 7)) = fields.get("code").copied() else {
                return ToolOutcome::Interrupted;
            };
            let release = cx.workspace().as_path().join(format!("release-{code}"));
            let label = format!("procs-proof-output-{code}");
            let argv: Vec<std::ffi::OsString> = vec![
                "/bin/sh".into(), "-c".into(),
                "while [ ! -f \"$1\" ]; do /bin/sleep 0.02; done; printf '%s\\n' \"$2\"; exit \"$3\"".into(),
                "procs-proof".into(), release.into_os_string(), label.into(), code.to_string().into(),
            ];
            let approved = match cx
                .authorize(dal_core::Preview {
                    title: "Bounded local proof child".into(),
                    body: "Two local shell children, eight-second Host timeout".into(),
                    digest: None,
                })
                .await
            {
                Ok(proof) => proof,
                Err(reason) => {
                    return ToolOutcome::Err(dal_agent::error::ToolError::Denied(reason));
                }
            };
            let opts = dal_agent::SpawnOpts {
                cwd: cx.workspace().as_path().to_path_buf(),
                timeout: Some(Duration::from_secs(8)),
                env: Vec::new(),
                stdout_prefix_limit: 1024,
            };
            match cx.spawn(&argv, opts, approved) {
                Ok(proc) => ToolOutcome::Detached(cx.detach(proc)),
                Err(error) => ToolOutcome::Err(error),
            }
        })
    }
}

// Opens the owner/isolated proof sessions and captures each session's
// Host-minted jobs service plus caller.
async fn open_proof_sessions(
    host: &dal_agent::Host,
    root: &Path,
    receiver: &mut tokio::sync::mpsc::Receiver<BoundServices>,
) -> Result<(Vec<dal_agent::Agent>, Vec<BoundServices>), SmokeError> {
    let mut agents = Vec::new();
    let mut handles = Vec::new();
    for name in ["owner", "isolated"] {
        let agent = host
            .open(
                dal_agent::SessionRef::New {
                    workspace: dal_core::Workspace::new(root.to_path_buf())
                        .map_err(|error| fail(error.to_string()))?,
                    name: Some(name.into()),
                },
                dal_core::ClientId::new(format!("procs-proof-{name}")),
            )
            .await
            .map_err(|error| fail(format!("open: {error}")))?;
        agent
            .submit(dal_core::Command::Run {
                name: "procs-proof-capture".into(),
                args: "".into(),
                expected: None,
            })
            .await
            .map_err(|error| fail(format!("capture: {error}")))?;
        handles.push(
            receiver
                .recv()
                .await
                .ok_or_else(|| fail("capture channel closed"))?,
        );
        agents.push(agent);
    }
    Ok((agents, handles))
}

// Drives one proof turn on the owner session and asserts the turn dispatched
// the proof child and ended cleanly.
async fn drive_proof_turn(owner_agent: &dal_agent::Agent, code: i32) -> Result<(), SmokeError> {
    let mut subscription = owner_agent
        .subscribe(None)
        .map_err(|error| fail(format!("subscribe: {error}")))?;
    let accepted = owner_agent
        .submit(dal_core::Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: format!("run proof child code {code}").into(),
            }],
        })
        .await
        .map_err(|error| fail(format!("prompt: {error}")))?;
    if !matches!(accepted, Reply::Accepted { .. }) {
        return Err(fail(format!("prompt not accepted: {accepted:?}")));
    }
    let mut saw_child = false;
    let mut saw_turn_end = false;
    let mut kinds: Vec<String> = Vec::new();
    let waited = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(delivery) = subscription.next().await {
            match delivery {
                Delivery::Resync { .. } => {
                    kinds.push("Resync".to_string());
                }
                Delivery::Update(update) => {
                    kinds.push(format!("{:?}", update.kind).chars().take(200).collect());
                    match &update.kind {
                        UpdateKind::ToolStarted { tool, .. }
                            if tool.to_string() == "procs-proof-child" =>
                        {
                            saw_child = true;
                        }
                        UpdateKind::TurnEnded { stop, .. } => {
                            if *stop != Stop::EndTurn {
                                return Err(fail(format!("turn ended {stop:?}")));
                            }
                            saw_turn_end = true;
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok::<(), SmokeError>(())
    })
    .await;
    println!(
        "procs-proof-diag code={code} reply={accepted:?} kinds={kinds:?} saw_child={saw_child} saw_turn_end={saw_turn_end}"
    );
    waited.map_err(|_| {
        fail(format!(
            "turn wait exceeded twenty seconds; kinds={kinds:?}"
        ))
    })??;
    if !saw_turn_end {
        return Err(fail(format!(
            "subscription closed without turn terminal; kinds={kinds:?}"
        )));
    }
    if !saw_child {
        return Err(fail(format!(
            "turn never dispatched procs-proof-child; kinds={kinds:?}"
        )));
    }
    Ok(())
}

// Reads the running table through the registered procs tool and claims the one
// running child job id.
async fn claim_running_child(
    tool: &Arc<dyn Tool>,
    owner: &BoundServices,
) -> Result<dal_core::JobId, SmokeError> {
    let running_probe = procs_text(tool, owner, r#"{"op":"list","status":"running"}"#).await?;
    let rows: Vec<&str> = running_probe
        .lines()
        .filter(|line| line.contains(" running "))
        .collect();
    let [row] = rows.as_slice() else {
        return Err(fail(format!(
            "want one running row, saw {}: {running_probe}",
            rows.len()
        )));
    };
    if !row.contains("procs-proof-child") {
        return Err(fail(format!("running row is not our child: {row}")));
    }
    let id_text = row
        .split_whitespace()
        .next()
        .ok_or_else(|| fail("running row has no id"))?;
    dal_core::JobId::parse(id_text).map_err(|error| fail(format!("job id: {error}")))
}

// Releases the child, waits for its terminal phase, and checks output capture
// plus session isolation on both ends.
async fn settle_proof_child(
    tool: &Arc<dyn Tool>,
    owner: &BoundServices,
    isolated: &BoundServices,
    root: &Path,
    id: dal_core::JobId,
    code: i32,
) -> Result<(), SmokeError> {
    let running = procs_text(tool, owner, r#"{"op":"list","status":"running"}"#).await?;
    if !running.contains(&format!("{id} running ")) {
        return Err(fail(format!("running identity missing: {running}")));
    }
    if procs_text(tool, isolated, r#"{"op":"list"}"#).await? != EXPECTED_EMPTY {
        return Err(fail("running Host process leaked across sessions"));
    }
    tokio::fs::write(root.join(format!("release-{code}")), b"release")
        .await
        .map_err(|error| fail(error.to_string()))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let phase = if code == 0 { "done" } else { "failed" };
    let row = format!("{id} {phase} ");
    loop {
        let terminal = procs_text(tool, owner, r#"{"op":"list"}"#).await?;
        if terminal.contains(&row) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(fail(format!(
                "Host failed to settle process {id}: {terminal}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let filtered = procs_text(
        tool,
        owner,
        &format!(r#"{{"op":"list","status":"{phase}"}}"#),
    )
    .await?;
    if !filtered.contains(&row) {
        return Err(fail(format!(
            "Host status filter omitted settled process {id}: {filtered}"
        )));
    }
    let label = format!("procs-proof-output-{code}");
    let grep = procs_text(
        tool,
        owner,
        &format!(r#"{{"op":"grep","query":"{label}"}}"#),
    )
    .await?;
    if !grep.contains(&format!("{id}:1: {label}")) {
        return Err(fail(format!(
            "Host captured process output missing: {grep}"
        )));
    }
    if procs_text(tool, isolated, r#"{"op":"list"}"#).await? != EXPECTED_EMPTY {
        return Err(fail("terminal Host process leaked across sessions"));
    }
    Ok(())
}

async fn host_process_proof(
    host: &dal_agent::Host,
    tool: &Arc<dyn Tool>,
    root: &Path,
    receiver: &mut tokio::sync::mpsc::Receiver<BoundServices>,
) -> Result<(), SmokeError> {
    let (agents, handles) = open_proof_sessions(host, root, receiver).await?;
    let owner = &handles[0];
    let isolated = &handles[1];
    let owner_agent = &agents[0];
    for code in [0, 7] {
        drive_proof_turn(owner_agent, code).await?;
        let id = claim_running_child(tool, owner).await?;
        let result = settle_proof_child(tool, owner, isolated, root, id, code).await;
        if result.is_err() {
            let _ = owner.0.jobs(&owner.1, JobsOp::Cancel { id }).await;
        }
        result?;
    }
    println!(
        "procs-smoke host-process-jobs: PASS Host spawn/detach/reap; identity/output/isolation read through the registered procs tool over the real Host jobs table"
    );
    println!(
        "procs-smoke procs-own-grant-path: UNPROVEN not exercised; procs ran under ToolCx::for_test with List/Text forwarded under the proof extension's Builtin jobs authority"
    );
    Ok(())
}

fn fixture_step(code: i32) -> String {
    const TEMPLATE: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"CALLID\",\"name\":\"procs-proof-child\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"CALLID\",\"name\":\"procs-proof-child\",\"args\":{\"kind\":\"parsed\",\"value\":{\"code\":CODE}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":4,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";
    TEMPLATE
        .replace("CALLID", &format!("call-proof-{code}"))
        .replace("CODE", &code.to_string())
}

const FIXTURE_FOLLOW: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"proof turn complete\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":12,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

// Writes the two-round scripted provider fixture (tool step + follow step per
// proof code) and returns its path.
async fn write_proof_fixture(root: &Path) -> Result<PathBuf, SmokeError> {
    let fixture = root.join("scripted-proof.jsonl");
    // One shared scripted queue serves every provider open of the Host, so each
    // prompt must see exactly [tool step, follow step]: the tool step is the
    // model request of round 0, the follow step the round after the detached call.
    // Unrelated automatic machinery is configured off below so that nothing else
    // opens the provider or wakes a turn.
    let mut steps = String::new();
    for code in [0, 7] {
        steps.push_str(&fixture_step(code));
        steps.push_str(FIXTURE_FOLLOW);
    }
    tokio::fs::write(&fixture, steps)
        .await
        .map_err(|error| fail(error.to_string()))?;
    Ok(fixture)
}

// Loads the proof product with orchestration disabled and judged thinking off,
// and resolves the registered procs tool.
fn load_proof_product(
    root: &Path,
    fixture: &Path,
) -> Result<(dal_core::Config, dal_agent::Product, Arc<dyn Tool>), SmokeError> {
    let factory = dalgona::product();
    // `[plugin.judged] thinking = false`: judged's before-request hook (round 0, once
    // per turn) otherwise infers a thinking level through the provider and takes the
    // queue's tool step. `disabled_batteries = ["orchestration"]`: its owner parks on a
    // capability Grant that nothing answers here (hook budget 5s, fail-closed) and it
    // wakes the model after job completion. Both are documented switches; `procs`,
    // the Host jobs service and the reaper are unaffected.
    let user = format!(
        "model = \"openai-responses/gpt-6\"\napproval = \"all\"\ndisabled_batteries = [\"orchestration\"]\n[plugin.judged]\nthinking = false\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config =
        dal_core::Config::load(ConfigProduct::Dalgona, root, factory.defaults, Some(&user))
            .map_err(|error| fail(format!("host config: {error:?}")))?;
    let cx = dalgon::BuildCx {
        data_root: root.to_path_buf(),
        config: &config,
    };
    let product = dalgona::build(&cx).map_err(|error| fail(format!("host build: {error:?}")))?;
    if product
        .extensions
        .iter()
        .any(|extension| extension.name() == "orchestration")
    {
        return Err(fail("host build still registers orchestration"));
    }
    let procs_tool = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "procs")
        .and_then(|extension| extension.tools().first().map(|(tool, _)| Arc::clone(tool)))
        .ok_or_else(|| fail("host build registers no procs tool"))?;
    Ok((config, product, procs_tool))
}

// Registers the capture command and the bounded proof child tool on the product.
fn push_proof_extensions(
    product: &mut dal_agent::Product,
    sender: tokio::sync::mpsc::Sender<BoundServices>,
) -> Result<(), SmokeError> {
    let capture = dal_agent::ext::ExtensionBuilder::new(
        "procs-proof",
        "0.0.0",
        dal_core::ext::ServiceSet::from_names(["jobs"]).map_err(|error| fail(error.to_string()))?,
    )
    .map_err(|error| fail(error.to_string()))?
    .with_origin(Origin::Builtin, None)
    .command(
        dal_core::CommandSpec {
            name: dal_core::CommandName::parse("procs-proof-capture")
                .map_err(|error| fail(error.to_string()))?,
            summary: "Capture an owned proof session".into(),
            args_hint: None,
        },
        Arc::new(Capture(sender)),
    )
    .build()
    .map_err(|error| fail(error.to_string()))?;
    product.extensions.push(capture);
    product.extensions.push(
        dal_agent::ext::ExtensionBuilder::new(
            "procs-proof-process",
            "0.0.0",
            dal_core::ext::ServiceSet::EMPTY,
        )
        .map_err(|error| fail(error.to_string()))?
        .with_origin(Origin::Builtin, None)
        .tool(
            Arc::new(HostChildTool::new(
                dal_core::Name::parse("procs-proof-child")
                    .map_err(|error| fail(error.to_string()))?,
            )?),
            Visibility::Model,
        )
        .build()
        .map_err(|error| fail(error.to_string()))?,
    );
    Ok(())
}

async fn check_host(base: &Path) -> Result<(), SmokeError> {
    let root = base.join("host");
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|error| fail(format!("host root: {error:?}")))?;
    let fixture = write_proof_fixture(&root).await?;
    let (config, mut product, procs_tool) = load_proof_product(&root, &fixture)?;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
    push_proof_extensions(&mut product, sender)?;
    let env = dal_agent::Env {
        vars: BTreeMap::new(),
        cwd: root.clone(),
        sandbox_helper: None,
    };
    let host = dal_agent::Host::start(product, config, env)
        .await
        .map_err(|error| fail(format!("host start: {error:?}")))?;
    let proof = tokio::time::timeout(Duration::from_secs(50), async {
        let procs_doc = host
            .doc("dalgona://procs")
            .map_err(|error| fail(format!("host procs doc: {error:?}")))?;
        if !format!("{procs_doc:?}").contains("background jobs") {
            return Err(fail("host dalgona://procs omits background jobs"));
        }
        let symbols_doc = host
            .doc("dalgona://symbols")
            .map_err(|error| fail(format!("host symbols doc: {error:?}")))?;
        if !format!("{symbols_doc:?}").contains("dal://symbols") {
            return Err(fail("host dalgona://symbols omits dal://symbols"));
        }
        host_process_proof(&host, &procs_tool, &root, &mut receiver).await
    })
    .await;
    let report = host.shutdown(Duration::from_secs(10)).await;
    proof.map_err(|_| fail("host process proof exceeded fifty seconds"))??;
    if report.sessions_closed != 2 {
        return Err(fail(format!(
            "shutdown closed {} sessions",
            report.sessions_closed
        )));
    }
    println!("procs-smoke real-host: PASS served pages, clean shutdown");
    Ok(())
}

fn run() -> Result<(), SmokeError> {
    if !cfg!(target_os = "linux") {
        return Err(fail(
            "this disposable process proof requires Linux and /bin/sh; no native-platform proof is claimed",
        ));
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| fail(error.to_string()))?
        .as_nanos();
    let base = std::env::temp_dir().join(format!("procs-smoke-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&base).map_err(|error| fail(format!("tmp root: {error:?}")))?;
    let result = run_all(&base);
    let _ = std::fs::remove_dir_all(&base);
    result
}

fn run_all(base: &Path) -> Result<(), SmokeError> {
    let product = build_product(base.join("default"), None)?;
    check_default(&product)?;
    let procs = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "procs")
        .ok_or_else(|| fail("procs extension vanished"))?;
    let tool = procs
        .tools()
        .first()
        .map(|(tool, _)| Arc::clone(tool))
        .ok_or_else(|| fail("procs registers no tool"))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| fail(format!("runtime: {error:?}")))?;
    runtime.block_on(check_empty_list(&tool))?;
    check_disabled(base)?;
    runtime.block_on(check_host(base))?;
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("procs-smoke FAIL: {error}");
            ExitCode::FAILURE
        }
    }
}
