//! Session-scoped extension services: capability gates plus backend dispatch.
//!
//! [`SessionServices`] is the [`Services`] implementation handed to hook and
//! handler contexts. Every gated method runs the same three steps in order:
//! the inject check ([`SessionServices::inject_remedy`] renders the exact
//! `dal.plugin` remedy for the denial), the [`GrantStore`] capability gate
//! with the `key().allows(service)` defense, then the backend. There is no
//! second gate anywhere: the Starlark bridge, the approval ladder, and the
//! process launcher enforce their own scopes on top of these decisions.
//!
//! Method routing:
//! - `fs_read`, `fs_write`, `net`, `agents`, `jobs`, `turn`, `sidecar`,
//!   `call_tool`, and `notify` delegate to the [`SessionBackend`] supplied at
//!   construction. The backend performs no inject or grant checks.
//! - `run` gates, then runs the exec approval ladder and the shared checked
//!   launcher through [`ToolCxRuntime`], the same seam [`ToolCx::spawn`] uses.
//!   Call-scoped grants, prefix and root enforcement, and job-end revocation
//!   live in that authorizing layer, never here.
//! - `ask` has no grant: answers resolve through the session [`Broker`].
//! - `env` reads one key through the backend; the signature cannot enumerate.
//! - `mcp` fails closed with `Denied(Unavailable)` when no client is set.
//! - `infer` and `infer_stream` are the trusted Rust surface: no inject check
//!   and no grant. Scripts never reach them; the Starlark `infer` slot calls
//!   [`SessionServices::script_infer`], which gates `Service::Infer` first.
//! - `append_record` journals one record on the current leaf through the
//!   session actor before it answers; `records` reads the caller's own rows
//!   on the current leaf path from the actor's published snapshot. No other
//!   extension's rows are visible.
//!
//! The run-output mapping leaves `stderr_tail` empty until the process
//! layer exposes a separate stderr tail.

use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use super::generation::Generation;
use super::grants::GrantStore;
use super::mcp::McpClient;
use super::overlay::Overlay;
use super::tool::{RawValue, Tool, ToolCxRuntime, ToolOutcome};
use super::{Caller, CallerKind};
use crate::Broker;
use crate::broker::{Resolution, Settled};
use crate::error::{SIDECAR_VALUE_LIMIT, ServiceError, ToolError};
use crate::proc::{ProcResult, ProcStatus, SpawnOpts};
use dal_core::ExitStatusKind;
use dal_core::ext::{McpDeclaration, McpRequest, McpResponse};
use dal_core::{
    AgentsOp, AgentsReply, Answer, CallId, ClientId, DenyReason, EntryId, FetchRequest,
    FetchResponse, Inference, JobsOp, JobsReply, ModelRequest, Name, Notice, Origin, Owner,
    Preview, Question, Request, RequestId, RunOutput, RunRequest, Service, SidecarOp, Site,
    StateError, StateNs, StateOp, StateRecord, TurnOp, TurnOpReply, Visibility, Workspace,
};
use dal_provider::EventStream;
use tokio::sync::watch;
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;

mod contract;
pub use contract::{ServiceFuture, Services};
pub(crate) use contract::{SessionBackend, SessionServicesDeps};

/// Session-scoped [`Services`] over one shared capability gate.
///
/// The host builds one of these per session and hands it out as
/// `Arc<dyn Services>`; the type itself never leaves the crate except for
/// [`SessionServices::script_infer`] and [`SessionServices::inject_remedy`],
/// which the Starlark bridge calls directly.
pub struct SessionServices {
    grants: Arc<GrantStore>,
    broker: Arc<Broker>,
    backend: Arc<dyn SessionBackend>,
    rt: Arc<dyn ToolCxRuntime>,
    mcp_client: Option<Arc<dyn McpClient>>,
    generation: watch::Receiver<Arc<Generation>>,
    overlay: Arc<Overlay>,
    history: Arc<[String]>,
    sites: HashMap<Name, Option<Site>>,
    cancel: CancellationToken,
    ask_timeout: Duration,
    ephemeral: bool,
    workspace: Workspace,
    ask_open: Mutex<Option<RequestId>>,
    next_call: AtomicU64,
}

/// Releases the session's single open-ask slot on drop and resolves the
/// broker request as `Cancel`, so every exit from `ask` — including the
/// caller dropping the future — frees the next ask and retires the
/// question it published. A stranded slot would deny every later ask as
/// busy; a stranded request would stay answerable on every front end
/// while nobody consumes its reply. The withdrawal is the request's first
/// resolution: the broker mark here wins late answers, and the actor
/// journals it so the fold broadcasts the retirement after the record.
struct AskSlot<'a> {
    slot: &'a Mutex<Option<RequestId>>,
    broker: &'a Broker,
    backend: Arc<dyn SessionBackend>,
    request: Request,
    armed: bool,
}

impl Drop for AskSlot<'_> {
    fn drop(&mut self) {
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let Some(resolved) = self.withdrawn() else {
            return;
        };
        // Bounded shed, no detached task: a full command queue drops the
        // report, and the broker mark above already denied late answers.
        self.backend.request_resolved(resolved);
    }
}

impl AskSlot<'_> {
    /// Marks the withdrawal in the broker and renders the resolution to
    /// journal, or `None` when the guard was disarmed or the request was
    /// already resolved.
    fn withdrawn(&mut self) -> Option<crate::broker::Resolved> {
        if !self.armed {
            return None;
        }
        let by = ClientId::new("core");
        let answered = self
            .broker
            .answer(self.request.id, Answer::Cancel, by.clone())
            .is_ok();
        answered.then(|| crate::broker::Resolved {
            request: self.request.clone(),
            answer: Answer::Cancel,
            by,
            resolution: Resolution::Cancelled,
            was_default: false,
        })
    }
}

impl SessionServices {
    /// Derives the caller's state namespace (R08): eval cells share the
    /// session eval namespace; every other caller owns the namespace its
    /// extension's `origin`, name, and `state_version` isolate.
    pub(crate) fn state_ns(who: &Caller) -> StateNs {
        if who.cell() {
            return StateNs::Eval;
        }
        // The caller carries its extension's `state_version` minted at
        // dispatch: an in-flight invocation keeps the namespace its own
        // generation snapshot gave it across a plugin reload.
        StateNs::Plugin {
            origin: who.origin(),
            plugin: who.ext().clone(),
            version: who.state_version(),
        }
    }

    /// Builds the session services from host-owned pieces.
    pub(crate) fn new(deps: SessionServicesDeps) -> Self {
        let grants = deps.grants;
        let backend = deps.backend;
        let update_backend = Arc::clone(&backend);
        grants.set_request_update(Arc::new(move |update| {
            update_backend.publish_update(update);
        }));
        let open_backend = Arc::clone(&backend);
        grants.set_request_open(Arc::new(move |request| {
            let backend = Arc::clone(&open_backend);
            Box::pin(async move {
                let _ = backend.request_opened(request).await;
            })
        }));
        Self {
            grants,
            broker: deps.broker,
            backend,
            rt: deps.rt,
            mcp_client: deps.mcp_client,
            generation: deps.generation,
            overlay: deps.overlay,
            history: deps.history,
            sites: deps.sites,
            cancel: deps.cancel,
            ask_timeout: deps.ask_timeout,
            ephemeral: deps.ephemeral,
            workspace: deps.workspace,
            ask_open: Mutex::new(None),
            next_call: AtomicU64::new(1),
        }
    }

    /// Renders the exact not-injected remedy for `service` and `who`.
    ///
    /// The [`ServiceError::Denied`] variant carries no span data, so callers
    /// that report the denial render this text next to it. A missing
    /// declaration span renders as `unknown`.
    #[must_use]
    pub fn inject_remedy(&self, who: &Caller, service: Service) -> String {
        let site = self
            .sites
            .get(&who.ext)
            .and_then(|site| site.as_ref())
            .map_or_else(
                || String::from("unknown"),
                |site| format!("{}:{}:{}", site.path.display(), site.line, site.col),
            );
        format!(
            "service \"{}\" is not declared for plugin \"{}\"; \
             declare the operation in the tool's `uses` (the v1 contract) at {site} \
             and approve the new grant when asked",
            service.as_str(),
            who.ext.as_str(),
        )
    }

    /// Runs the paid script `infer` slot: inject check, capability gate,
    /// then the backend. Unlike [`Services::infer`], never trusted.
    ///
    /// # Errors
    ///
    /// Returns the inject, grant, or backend failure.
    #[must_use]
    pub fn script_infer(&self, who: &Caller, req: ModelRequest) -> ServiceFuture<'_, Inference> {
        self.script_infer_with(who, req, None, self.cancel.clone())
    }

    pub(crate) fn script_infer_with(
        &self,
        who: &Caller,
        req: ModelRequest,
        script: Option<Arc<crate::session::script::SessionScriptHost>>,
        cancel: CancellationToken,
    ) -> ServiceFuture<'_, Inference> {
        let who = who.clone();
        Box::pin(async move {
            Self::check_inject(&who, Service::Infer)?;
            self.gated_with(&who, Service::Infer, cancel.clone())
                .await?;
            self.backend.infer_with_script(req, script, cancel).await
        })
    }

    /// Rejects callers that did not inject `service`, before any grant read.
    fn check_inject(who: &Caller, service: Service) -> Result<(), ServiceError> {
        if who.inject.contains(service) {
            Ok(())
        } else {
            Err(ServiceError::Denied(DenyReason::NotInjected))
        }
    }

    /// Runs the shared capability gate with the `allows` defense.
    ///
    /// Cell callers carry their eval approval and builtin extensions carry
    /// none of the grant machinery, so both skip the store exactly like
    /// [`GrantStore::ensure`] does internally.
    fn gated(&self, who: &Caller, service: Service) -> ServiceFuture<'_, ()> {
        self.gated_with(who, service, self.cancel.clone())
    }

    fn gated_with(
        &self,
        who: &Caller,
        service: Service,
        cancel: CancellationToken,
    ) -> ServiceFuture<'_, ()> {
        let who = who.clone();
        Box::pin(async move {
            if matches!(who.kind, CallerKind::Cell { .. }) || who.origin == Origin::Builtin {
                return Ok(());
            }
            let grant = self
                .grants
                .ensure(&who, service, &cancel)
                .await
                .map_err(|error| error.naming_grant(service, who.ext.as_str()))?;
            if grant.key().allows(service) {
                Ok(())
            } else {
                Err(ServiceError::service_not_granted(service, who.ext.as_str()))
            }
        })
    }

    /// Builds the exec approval preview for one run request.
    fn run_preview(&self, req: &RunRequest) -> Preview {
        let argv: Vec<String> = req
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let command = if argv.is_empty() {
            String::from("run")
        } else {
            argv.join(" ")
        };
        let cwd = req
            .cwd
            .as_deref()
            .unwrap_or_else(|| self.workspace.as_path());
        Preview {
            title: format!("run {}", argv.first().map_or("run", String::as_str)).into(),
            body: format!("cwd: {}\ncmd: {command}", cwd.display()).into(),
            digest: None,
        }
    }

    /// Maps a launcher failure onto the service error surface.
    fn run_failure(error: ToolError) -> ServiceError {
        match error {
            ToolError::Cancelled => ServiceError::Cancelled,
            ToolError::Denied(reason) => ServiceError::Denied(reason),
            other => ServiceError::failed(Some(Service::Run), other.to_string()),
        }
    }

    /// Maps a ladder denial onto the service error surface. A call with no
    /// one to ask fails closed with the shared headless text that names
    /// the fix, exactly like a gated tool call.
    fn map_deny(reason: DenyReason) -> ServiceError {
        ServiceError::Denied(match reason {
            DenyReason::NoFrontEnd => {
                DenyReason::out_of_scope(dal_core::headless_denial_text("run", dal_core::Rung::All))
            }
            reason => reason,
        })
    }

    /// Opens one exec approval for a services `run` call and runs it after
    /// approval, exactly like the exec tool ladder: answerable by attached
    /// front ends, fail closed with a clear message when none can answer.
    async fn run_with_approval(
        &self,
        who: &Caller,
        call: &CallId,
        preview: Preview,
        req: RunRequest,
    ) -> Result<RunOutput, ServiceError> {
        let Some(turn) = who.turn else {
            return Err(ServiceError::Denied(DenyReason::out_of_scope(
                dal_core::headless_denial_text("run", dal_core::Rung::All),
            )));
        };
        let question = Question::Approval {
            tool: "run".into(),
            preview: preview.clone(),
            grant: None,
            call: Some(call.clone()),
        };
        let secs = crate::broker::default_timeout(&question).as_secs();
        let owner = Owner::Extension {
            name: who.ext.as_str().into(),
            origin: origin_name(who.origin).into(),
        };
        let deadline = Instant::now() + crate::broker::default_timeout(&question);
        let (request, waiter) = self.broker.open(owner, question, turn, deadline);
        self.backend
            .request_opened(request.clone())
            .await
            .map_err(|error| ServiceError::failed(Some(Service::Run), error.to_string()))?;
        self.backend
            .publish_update(dal_core::UpdateKind::RequestOpened(request.clone()));
        let settled = tokio::select! {
            () = self.cancel.cancelled() => None,
            outcome = waiter => Some(outcome),
        };
        let Some(Settled {
            answer,
            by,
            resolution,
        }) = settled
        else {
            return Err(ServiceError::Cancelled);
        };
        match (resolution, answer) {
            (_, Answer::Approve | Answer::ApproveForSession) => {
                let approved = self
                    .rt
                    .authorize_approved(call, preview, &self.cancel)
                    .await
                    .map_err(Self::map_deny)?;
                self.run_spawned(req, approved).await
            }
            (_, Answer::Decline) => Err(ServiceError::Denied(DenyReason::out_of_scope(
                match resolution {
                    Resolution::Unavailable => format!(
                        "Permission denied: run needed approval and no one answered within {secs} s."
                    ),
                    Resolution::Answered | Resolution::Cancelled => {
                        format!("Permission denied: run was declined by {}.", by.as_str())
                    }
                },
            ))),
            (_, Answer::Cancel) => Err(ServiceError::Denied(DenyReason::Unavailable {
                what: "approval cancelled".into(),
            })),
            _ => Err(ServiceError::Denied(DenyReason::out_of_scope(
                "Permission denied: run.",
            ))),
        }
    }

    /// Spawns one approved services run and waits for its output.
    async fn run_spawned(
        &self,
        req: RunRequest,
        approved: crate::ext::tool::Approved,
    ) -> Result<RunOutput, ServiceError> {
        let opts = SpawnOpts {
            cwd: req
                .cwd
                .unwrap_or_else(|| self.workspace.as_path().to_path_buf()),
            timeout: req.timeout,
            env: req
                .env
                .into_iter()
                .map(|(key, value)| {
                    (
                        std::ffi::OsString::from(String::from(key)),
                        std::ffi::OsString::from(String::from(value)),
                    )
                })
                .collect(),
            stdout_prefix_limit: usize::try_from(req.stdout_prefix_limit).unwrap_or(usize::MAX),
        };
        let mut child = self
            .rt
            .spawn(&req.argv, opts, approved)
            .map_err(Self::run_failure)?;
        let result = child.wait(&self.cancel).await.map_err(Self::run_failure)?;
        Ok(run_output_of(&result))
    }
}

/// Maps a finished child onto the run service output.
///
/// `stderr_tail` stays empty: [`ProcResult`] exposes no separate stderr
/// tail, and the merged completion tail must never print as stderr.
fn run_output_of(result: &ProcResult) -> RunOutput {
    RunOutput {
        status: match result.status {
            ProcStatus::Exited { code } => ExitStatusKind::Exited(code),
            ProcStatus::Signaled { signal } => ExitStatusKind::Signaled(signal),
            ProcStatus::TimedOut => ExitStatusKind::TimedOut,
            ProcStatus::Cancelled => ExitStatusKind::Aborted,
        },
        stdout_tail: result.preview.to_vec(),
        stdout_prefix: result.stdout_prefix.to_vec(),
        stdout_prefix_overflowed: result.stdout_prefix_overflowed,
        stderr_tail: Vec::new(),
        log: Some(result.log_path.clone()),
    }
}

/// Fixed surface text for the missing-MCP-client denial.
///
/// [`Services::mcp`] returns the machine-readable
/// `Denied(Unavailable { what: "MCP client" })`; front ends render this
/// text next to it.
pub const MCP_UNAVAILABLE_TEXT: &str =
    "the mcp service needs an MCP client; dalgon has none; dalgona configures one under [mcp]";

/// Fixed surface text for sidecar use in an ephemeral session.
///
/// [`Services::sidecar`] returns the machine-readable
/// `Denied(Unavailable { what: "sidecar (ephemeral session)" })`; front ends render this text
/// next to it.
pub const SIDECAR_EPHEMERAL_TEXT: &str = "sidecar is unavailable for ephemeral sessions";

fn origin_name(origin: Origin) -> &'static str {
    match origin {
        Origin::Bundled => "bundled",
        Origin::User => "user",
        Origin::Builtin => "builtin",
        _ => "unknown",
    }
}

impl Services for SessionServices {
    fn fs_read(&self, who: &Caller, path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        let who = who.clone();
        let path = path.to_owned();
        Box::pin(async move {
            Self::check_inject(&who, Service::FsRead)?;
            self.gated(&who, Service::FsRead).await?;
            self.backend.fs_read(&path).await
        })
    }

    fn fs_write(&self, who: &Caller, path: &str, bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        let who = who.clone();
        let path = path.to_owned();
        Box::pin(async move {
            Self::check_inject(&who, Service::FsWrite)?;
            self.gated(&who, Service::FsWrite).await?;
            self.backend.fs_write(&path, bytes).await
        })
    }

    fn net(&self, who: &Caller, req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        let who = who.clone();
        Box::pin(async move {
            Self::check_inject(&who, Service::Net)?;
            self.gated(&who, Service::Net).await?;
            self.backend.net(req).await
        })
    }

    fn run(&self, who: &Caller, req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        let who = who.clone();
        Box::pin(async move {
            Self::check_inject(&who, Service::Run)?;
            self.gated(&who, Service::Run).await?;
            req.validate_env()
                .map_err(|error| ServiceError::failed(Some(Service::Run), error.to_string()))?;
            let preview = self.run_preview(&req);
            let call = self.next_call_id();
            let covered = self
                .rt
                .covered_run(&who, &call, &req, &preview, &self.cancel)
                .await
                .map_err(Self::map_deny)?;
            if let Some(approved) = covered {
                return self.run_spawned(req, approved).await;
            }
            match self.rt.decide_run() {
                dal_core::Decision::Allow => {
                    let approved = self
                        .rt
                        .authorize(&call, preview, &self.cancel)
                        .await
                        .map_err(Self::map_deny)?;
                    self.run_spawned(req, approved).await
                }
                dal_core::Decision::Deny { reason } => Err(Self::map_deny(reason)),
                dal_core::Decision::Ask { .. } => {
                    self.run_with_approval(&who, &call, preview, req).await
                }
                _ => Err(ServiceError::Denied(DenyReason::out_of_scope("run"))),
            }
        })
    }

    fn env(&self, who: &Caller, key: &str) -> ServiceFuture<'_, Option<String>> {
        let who = who.clone();
        let key = key.to_owned();
        Box::pin(async move {
            Self::check_inject(&who, Service::Env)?;
            self.gated(&who, Service::Env).await?;
            Ok(self.backend.env(&key))
        })
    }

    fn ask(&self, who: &Caller, question: Question) -> ServiceFuture<'_, Option<Answer>> {
        let who = who.clone();
        let confirm = matches!(question, Question::Confirm { .. });
        Box::pin(async move {
            Self::check_inject(&who, Service::Ask)?;
            let Some(turn) = who.turn else {
                return Err(ServiceError::turn_not_running());
            };
            // With no answering front end attached at the moment of raise
            // (print mode, --json, the router, A2A, child sessions), the
            // question takes its fail-closed default at once and no request
            // opens. A request raised while one is attached keeps waiting if
            // it detaches: only the absolute timeout ends it, so a client can
            // reattach.
            if !self.backend.answerer_attached() {
                return Ok(None);
            }
            // One guard across check, open, and set: `open` is synchronous,
            // so two concurrent asks cannot both slip through.
            let (request, answer) = {
                let mut open = self
                    .ask_open
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if open.is_some() {
                    return Err(ServiceError::ask_busy());
                }
                let owner = Owner::Extension {
                    name: who.ext.as_str().into(),
                    origin: origin_name(who.origin).into(),
                };
                let deadline = Instant::now() + self.ask_timeout;
                let (request, answer) = self.broker.open(owner, question, turn, deadline);
                *open = Some(request.id);
                (request, answer)
            };
            // The guard clears the slot on every exit — including the
            // caller dropping this future — so a cancellation can strand
            // neither the session's one open ask nor its broker request.
            // It guards the actor route too: a failed route drops the
            // guard, which withdraws the never-published question.
            let mut guard = AskSlot {
                slot: &self.ask_open,
                broker: &self.broker,
                backend: Arc::clone(&self.backend),
                request: request.clone(),
                armed: true,
            };
            self.backend
                .request_opened(request.clone())
                .await
                .map_err(|error| ServiceError::failed(Some(Service::Ask), error.to_string()))?;
            // Front ends learn a request exists only from this update:
            // without it the question is unanswerable and the caller
            // waits out the timeout for nothing.
            self.backend
                .publish_update(dal_core::UpdateKind::RequestOpened(request.clone()));
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => Err(ServiceError::Cancelled),
                () = sleep(self.ask_timeout) => Ok(None),
                Settled { answer, resolution, .. } = answer => {
                    guard.armed = false;
                    match (resolution, answer) {
                        // No controller answered: the fail-closed default,
                        // never a dismissal and never an interruption.
                        (Resolution::Unavailable, _) => Ok(None),
                        (_, value @ Answer::Value(_)) => Ok(Some(value)),
                        // Confirmation front ends answer with approve and
                        // decline rather than typed booleans; normalize
                        // both before the script sees them.
                        (_, Answer::Approve) if confirm => Ok(Some(Answer::Value(
                            dal_core::RawJson::parse("true")
                                .map_err(|error| ServiceError::failed(Some(Service::Ask), error.to_string()))?,
                        ))),
                        (_, Answer::Decline) if confirm => Ok(Some(Answer::Value(
                            dal_core::RawJson::parse("false")
                                .map_err(|error| ServiceError::failed(Some(Service::Ask), error.to_string()))?,
                        ))),
                        // Turn cancellation resolves the open request as
                        // `Cancel`; dismissal arrives as `Decline`.
                        (_, Answer::Cancel) => Err(ServiceError::Cancelled),
                        _ => Ok(None),
                    }
                },
            }
        })
    }

    fn mcp(&self, who: &Caller, req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        let who = who.clone();
        let client = self.mcp_client.clone();
        Box::pin(async move {
            Self::check_inject(&who, Service::Mcp)?;
            self.gated(&who, Service::Mcp).await?;
            let Some(client) = client else {
                return Err(ServiceError::Denied(DenyReason::Unavailable {
                    what: "MCP client".into(),
                }));
            };
            client.call(&who, req).await
        })
    }

    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        let generation = Arc::clone(&self.generation.borrow());
        Box::pin(async move { Ok(generation.mcp_declarations()) })
    }

    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        let texts = self.history.to_vec();
        Box::pin(std::future::ready(Ok(texts)))
    }

    fn add_session_tools(
        &self,
        who: &Caller,
        tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        let generation = Arc::clone(&self.generation.borrow());
        let owner = who.ext().clone();
        let staged = self.overlay.stage(&generation, &owner, tools);
        Box::pin(std::future::ready(staged))
    }

    fn agents(&self, who: &Caller, op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        let who = who.clone();
        Box::pin(async move {
            Self::check_inject(&who, Service::Agents)?;
            self.gated(&who, Service::Agents).await?;
            self.backend.agents(op).await
        })
    }

    fn jobs(&self, who: &Caller, op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        let who = who.clone();
        Box::pin(async move {
            Self::check_inject(&who, Service::Jobs)?;
            self.gated(&who, Service::Jobs).await?;
            let top_level = matches!(op, JobsOp::Spawn { parent: None, .. });
            let reply = self.backend.jobs(&who.ext, op).await?;
            if let (true, JobsReply::Spawned { id }) = (top_level, &reply) {
                self.rt.job_started(&who, *id);
            }
            Ok(reply)
        })
    }

    fn open_asks(&self, who: &Caller) -> ServiceFuture<'_, usize> {
        let who = who.clone();
        Box::pin(async move {
            Self::check_inject(&who, Service::Ask)?;
            let count = self
                .broker
                .open_requests()
                .iter()
                .filter(|request| {
                    matches!(
                        &request.question,
                        Question::Select { .. } | Question::Confirm { .. } | Question::Text { .. }
                    )
                })
                .count();
            Ok(count)
        })
    }

    fn scheme(&self, who: &Caller, uri: &str) -> ServiceFuture<'_, Option<crate::ext::Doc>> {
        let who = who.clone();
        let uri = uri.to_owned();
        Box::pin(async move { self.backend.scheme(&who, &uri).await })
    }

    fn turn(&self, who: &Caller, op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        let who = who.clone();
        Box::pin(async move {
            Self::check_inject(&who, Service::Turn)?;
            self.gated(&who, Service::Turn).await?;
            self.backend.turn(op).await
        })
    }

    fn sidecar(&self, who: &Caller, op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        let who = who.clone();
        Box::pin(async move {
            Self::check_inject(&who, Service::Sidecar)?;
            self.gated(&who, Service::Sidecar).await?;
            if self.ephemeral {
                return Err(ServiceError::Denied(DenyReason::Unavailable {
                    what: "sidecar (ephemeral session)".into(),
                }));
            }
            match op {
                SidecarOp::Read { name } => self.backend.sidecar_read(who.ext(), &name).await,
                SidecarOp::Write { name, bytes } => {
                    let size = bytes.len() as u64;
                    if size > SIDECAR_VALUE_LIMIT {
                        return Err(ServiceError::sidecar_too_large(name.as_str(), size));
                    }
                    self.backend
                        .sidecar_write(who.ext(), &name, bytes)
                        .await
                        .map(|()| None)
                }
                SidecarOp::Artifact { job, file, bytes } => self
                    .backend
                    .sidecar_artifact(job, file, bytes)
                    .await
                    .map(|()| None),
                _ => Err(ServiceError::failed(
                    Some(Service::Sidecar),
                    "unsupported sidecar operation",
                )),
            }
        })
    }

    fn state(
        &self,
        who: &Caller,
        op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        let who = who.clone();
        Box::pin(async move {
            Self::check_inject(&who, Service::Sidecar)?;
            self.gated(&who, Service::Sidecar).await?;
            let ns = Self::state_ns(&who);
            // The caller never names its namespace: it is derived here so a
            // script cannot reach another plugin's state.
            let op = match op {
                StateOp::Read { key, .. } => StateOp::Read { ns, key },
                StateOp::Write {
                    key,
                    value,
                    expected,
                    ..
                } => StateOp::Write {
                    ns,
                    key,
                    value,
                    expected,
                },
                StateOp::Delete { key, expected, .. } => StateOp::Delete { ns, key, expected },
            };
            self.backend.state(op).await
        })
    }

    fn infer(&self, _who: &Caller, req: ModelRequest) -> ServiceFuture<'_, Inference> {
        Box::pin(async move { self.backend.infer(req).await })
    }

    fn infer_stream(&self, _who: &Caller, req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        Box::pin(async move { self.backend.infer_stream(req).await })
    }

    fn call_tool(
        &self,
        _who: &Caller,
        name: &str,
        args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        // No inject or grant gate: the capability vocabulary names no
        // tool-dispatch service, and this Rust-only entry never reaches
        // scripts. The callee runs under `CallerKind::Tool`, so every
        // service it touches gates on the calling extension there.
        let name = name.to_owned();
        Box::pin(async move { self.backend.call_tool(&name, &args).await })
    }

    fn notify(&self, _who: &Caller, notice: Notice) {
        self.backend.notify(notice);
    }

    fn append_record(
        &self,
        who: &Caller,
        kind: &str,
        body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        let ext = who.ext.clone();
        let kind: Box<str> = kind.into();
        Box::pin(async move { self.backend.append_record(&ext, &kind, *body).await })
    }

    fn records(&self, who: &Caller, kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        let ext = who.ext.clone();
        let want: Box<str> = kind.into();
        Box::pin(async move {
            Ok(self
                .backend
                .ext_records()
                .iter()
                .filter(|record| record.ext == ext && record.kind == want)
                .map(|record| Box::new(record.body.clone()))
                .collect())
        })
    }

    fn blob_put(&self, _who: &Caller, bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        Box::pin(async move { self.backend.blob_put(bytes).await })
    }

    fn blob_get(&self, _who: &Caller, digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move { self.backend.blob_get(digest).await })
    }
}

impl SessionServices {
    /// Mints the call identity bound to one run authorization.
    fn next_call_id(&self) -> CallId {
        let n = self.next_call.fetch_add(1, Ordering::Relaxed);
        CallId::new(format!("services-run-{n}"))
    }
}

#[cfg(test)]
mod tests;
