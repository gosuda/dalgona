//! Session data-plane behind extension services.
//!
//! The backend performs ungated file, network, agent, job, turn, sidecar,
//! inference, tool, notice, and env operations; all capability gates live
//! in [`SessionServices`]. It also backs [`ToolCxRuntime`]: approval ladder,
//! process launch, job parking, scheme resolution, and workspace access.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dal_core::ext::Mail as ExtMail;
use dal_core::{
    AgentInfo, AgentReport, AgentState, AgentsOp, AgentsReply, BlobId, EntryId, FetchMethod,
    FetchRequest, FetchResponse, Inference, JobsOp, JobsReply, MailMode, ModelRequest, Name,
    Notice, Part, SessionId, TurnOp, TurnOpReply, Workspace,
};
use dal_provider::EventStream;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::broker::Broker;
use crate::error::{AgentError, ServiceError};
use crate::ext::ExtRecord;
use crate::ext::services::{ServiceFuture, SessionBackend, SessionServices};
use crate::ext::tool::RawValue;
use crate::host::HostState;
use crate::session::SessionHandle;
use crate::session::shared::Shared;
use crate::session::tasks::SessionTasks;

/// Inputs for one session data-plane.
pub(crate) struct BackendDeps {
    /// The session identity.
    pub(crate) session: SessionId,
    /// The explicit session workspace.
    pub(crate) workspace: Workspace,
    /// The host state for admission and process inputs.
    pub(crate) host: Arc<HostState>,
    /// The shared session snapshot.
    pub(crate) shared: Arc<Shared>,
    /// The entry snapshot captured when the session was opened.
    pub(crate) initial_entries: Arc<[dal_core::EntryView]>,
    /// The session request broker.
    pub(crate) broker: Arc<Broker>,
    /// The actor port for durable operations.
    pub(crate) handle: SessionHandle,
    /// The session job table.
    pub(crate) jobs: Arc<tokio::sync::Mutex<crate::jobs::JobTable>>,
    /// The session cancellation token.
    pub(crate) cancel: CancellationToken,
    /// The owner for background work in this session.
    pub(crate) tasks: SessionTasks,
}

fn record_session_closed() -> ServiceError {
    ServiceError::failed(None, "the session closed before the record was journaled.")
}

fn write_isolation_artifact(
    root: &Path,
    session: &str,
    task: &str,
    file: &str,
    bytes: &[u8],
) -> Result<(), std::io::Error> {
    let root = std::fs::canonicalize(root)?;
    let isolation = root.join("isolation");
    let session_dir = isolation.join(session);
    let task_dir = session_dir.join(task);
    for directory in [&isolation, &session_dir, &task_dir] {
        match std::fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(std::io::Error::other(
                    "artifact parent must not be a symlink",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                dal_store::create_private_dir_all(directory)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
            }
            Err(error) => return Err(error),
        }
        let canonical = std::fs::canonicalize(directory)?;
        if !canonical.starts_with(&root) {
            return Err(std::io::Error::other("artifact path escaped the data root"));
        }
    }
    let target = task_dir.join(file);
    dal_store::write_atomic(&target, bytes, dal_store::FileMode::Mode0600)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

/// Maps a typed wake refusal onto the service error the caller sees.
fn wake_service_error(reason: dal_core::ext::WakeError) -> ServiceError {
    match reason {
        dal_core::ext::WakeError::Limit => ServiceError::Denied(dal_core::DenyReason::WakeLimit),
        dal_core::ext::WakeError::Busy => ServiceError::failed(
            Some(dal_core::Service::Turn),
            "the session is busy: a turn or compaction is running",
        ),
        dal_core::ext::WakeError::Journal { message } => {
            ServiceError::failed(Some(dal_core::Service::Turn), message)
        }
        other => ServiceError::failed(
            Some(dal_core::Service::Turn),
            format!("the wake was refused: {other}"),
        ),
    }
}

fn session_root(
    sessions: &std::collections::HashMap<SessionId, crate::host::SessionEntry>,
    start: SessionId,
) -> Option<SessionId> {
    let mut current = start;
    for _ in 0..=sessions.len() {
        let entry = sessions.get(&current)?;
        match entry.parent {
            Some(parent) => current = parent,
            None => return Some(current),
        }
    }
    None
}

fn blob_id_from_digest(digest: [u8; 32]) -> Result<BlobId, ServiceError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = [0_u8; 64];
    for (index, byte) in digest.into_iter().enumerate() {
        encoded[index * 2] = HEX[usize::from(byte >> 4)];
        encoded[index * 2 + 1] = HEX[usize::from(byte & 0x0f)];
    }
    let encoded = std::str::from_utf8(&encoded)
        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
    BlobId::parse(encoded).map_err(|_| ServiceError::failed(None, "the blob digest is invalid"))
}

/// Host data-plane for one live session.
pub(crate) struct Backend {
    session: SessionId,
    workspace: Workspace,
    canonical_root: PathBuf,
    host: Arc<HostState>,
    shared: Arc<Shared>,
    initial_entries: Arc<[dal_core::EntryView]>,
    scheme_store: Arc<dal_store::Store>,
    broker: Arc<Broker>,
    handle: SessionHandle,
    services: Arc<std::sync::OnceLock<Arc<dyn crate::ext::Services>>>,
    script_services: std::sync::OnceLock<std::sync::Weak<SessionServices>>,
    jobs: Arc<tokio::sync::Mutex<crate::jobs::JobTable>>,
    procs: Arc<tokio::sync::Mutex<std::collections::HashMap<dal_core::JobId, crate::proc::Proc>>>,
    env_snapshot: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    launcher: Result<crate::proc::Launcher, crate::proc::sandbox::SandboxSetupError>,
    cancel: CancellationToken,
    tasks: SessionTasks,
    jobs_dir: PathBuf,
}

impl Backend {
    /// Builds the data-plane; the caller retains the session cancel token.
    pub(crate) fn new(deps: BackendDeps) -> Self {
        let BackendDeps {
            session,
            workspace,
            host,
            shared,
            initial_entries,
            broker,
            handle,
            jobs,
            cancel,
            tasks,
        } = deps;
        let canonical_root = std::fs::canonicalize(workspace.as_path())
            .unwrap_or_else(|_| workspace.as_path().to_path_buf());
        let env_snapshot = host
            .shared
            .env
            .vars
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let scheme_store = Arc::new(dal_store::Store::new(
            host.shared.data_root.clone(),
            workspace.clone(),
            super::commands::host::journal_product(host.shared.product_name),
        ));
        let jobs_dir = scheme_store.session_jobs_dir(session);
        let launcher = crate::proc::sandbox::session_launcher(
            &host.shared.config,
            &host.shared.env.vars,
            workspace.as_path(),
            std::slice::from_ref(&host.shared.data_root),
            host.shared.env.sandbox_helper.as_deref(),
        );
        Self {
            session,
            workspace,
            canonical_root,
            host,
            shared,
            initial_entries,
            scheme_store,
            broker,
            handle,
            services: Arc::new(std::sync::OnceLock::new()),
            script_services: std::sync::OnceLock::new(),
            jobs,
            procs: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            env_snapshot,
            launcher,
            cancel,
            tasks,
            jobs_dir,
        }
    }

    /// Returns the owning session.
    pub(crate) fn session(&self) -> SessionId {
        self.session
    }

    /// Borrows the owner for background work in this session.
    pub(crate) fn tasks(&self) -> &SessionTasks {
        &self.tasks
    }

    /// Borrows the session workspace.
    pub(crate) fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Borrows the store session `jobs/` directory for process witnesses.
    pub(crate) fn jobs_dir(&self) -> &std::path::Path {
        &self.jobs_dir
    }

    /// Borrows the session broker.
    pub(crate) fn broker(&self) -> &Arc<Broker> {
        &self.broker
    }
    /// Borrows the session-start entry snapshot for scheme resolution.
    pub(crate) fn initial_entries(&self) -> &Arc<[dal_core::EntryView]> {
        &self.initial_entries
    }

    /// Borrows the store shared by session scheme resolvers.
    pub(crate) fn scheme_store(&self) -> &Arc<dal_store::Store> {
        &self.scheme_store
    }
    /// Borrows the session snapshot used by extension-scheme contexts.
    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Borrows the actor port for durable command submission.
    pub(crate) fn handle(&self) -> &SessionHandle {
        &self.handle
    }

    /// Borrows the capability-scoped services cell set at session start.
    pub(crate) fn services(&self) -> &Arc<std::sync::OnceLock<Arc<dyn crate::ext::Services>>> {
        &self.services
    }

    /// Publishes the session services once the session start builds them.
    pub(crate) fn set_services(&self, services: &Arc<SessionServices>) {
        let _ = self
            .services
            .set(Arc::clone(services) as Arc<dyn crate::ext::Services>);
        let _ = self.script_services.set(Arc::downgrade(services));
    }

    pub(crate) fn script_services(&self) -> Option<Arc<SessionServices>> {
        self.script_services.get()?.upgrade()
    }

    /// Borrows the session job table.
    pub(crate) fn jobs(&self) -> &Arc<tokio::sync::Mutex<crate::jobs::JobTable>> {
        &self.jobs
    }

    /// Borrows the retained detached processes awaiting the jobs loop.
    pub(crate) fn procs(
        &self,
    ) -> &Arc<tokio::sync::Mutex<std::collections::HashMap<dal_core::JobId, crate::proc::Proc>>>
    {
        &self.procs
    }

    /// Borrows the captured environment snapshot for spawned children.
    pub(crate) fn env_snapshot(&self) -> &[(std::ffi::OsString, std::ffi::OsString)] {
        &self.env_snapshot
    }

    /// Borrows the process launcher prepared at session start; the error
    /// side refuses every spawn with the exact sandbox setup text.
    pub(crate) fn launcher(
        &self,
    ) -> &Result<crate::proc::Launcher, crate::proc::sandbox::SandboxSetupError> {
        &self.launcher
    }

    /// Borrows the host state for admission gates.
    pub(crate) fn host_state(&self) -> &Arc<HostState> {
        &self.host
    }
    /// Resolves the approval context of the nearest answerer-bearing
    /// session on the parent chain.
    ///
    /// A nested call runs on a child backend, but its ask must publish
    /// where the interactive client is subscribed; the walk starts at this
    /// session and climbs while no subscriber watches.
    pub(crate) fn answerer_context(&self) -> Option<(Arc<Broker>, Arc<Shared>)> {
        let sessions = self
            .host
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut current = self.session;
        for _ in 0..=sessions.len() {
            let entry = sessions.get(&current)?;
            if entry.shared.attached() {
                return Some((Arc::clone(&entry.broker), Arc::clone(&entry.shared)));
            }
            current = entry.parent?;
        }
        None
    }

    /// Borrows the session cancellation token.
    pub(crate) fn turn_cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Joins a workspace-relative path, refusing escapes from the root.
    fn contained(&self, path: &str) -> Option<PathBuf> {
        let joined = self.canonical_root.join(path);
        let resolved = if joined.exists() {
            std::fs::canonicalize(&joined).unwrap_or(joined)
        } else if let Some(parent) = joined.parent() {
            let canonical_parent =
                std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
            canonical_parent.join(joined.file_name()?)
        } else {
            joined
        };
        resolved
            .starts_with(&self.canonical_root)
            .then_some(resolved)
    }

    fn host(&self) -> crate::host::Host {
        crate::host::Host {
            state: Arc::clone(&self.host),
        }
    }
}

impl SessionBackend for Backend {
    fn fs_read(&self, path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        let resolved = self.contained(path);
        Box::pin(async move {
            let Some(resolved) = resolved else {
                return Ok(None);
            };
            Ok(tokio::fs::read(resolved).await.ok())
        })
    }

    fn fs_write(&self, path: &str, bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        let resolved = self.contained(path);
        Box::pin(async move {
            if let Some(resolved) = resolved {
                if let Some(parent) = resolved.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                let _ = tokio::fs::write(resolved, bytes).await;
            }
            Ok(())
        })
    }

    fn net(&self, req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        Box::pin(async move {
            let method = match req.method {
                FetchMethod::Post => reqwest::Method::POST,
                FetchMethod::Put => reqwest::Method::PUT,
                FetchMethod::Delete => reqwest::Method::DELETE,
                FetchMethod::Head => reqwest::Method::HEAD,
                FetchMethod::Options => reqwest::Method::OPTIONS,
                FetchMethod::Patch => reqwest::Method::PATCH,
                _ => reqwest::Method::GET,
            };
            let client = dal_provider::build_client();
            let mut outgoing = client.request(method, req.url.as_ref()).body(req.body);
            for (name, value) in req.headers {
                if let (Ok(name), Ok(value)) = (
                    reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                    reqwest::header::HeaderValue::from_str(&value),
                ) {
                    outgoing = outgoing.header(name, value);
                }
            }
            let failure = FetchResponse {
                status: 0,
                headers: Vec::new(),
                body: Vec::new(),
            };
            let Ok(response) = outgoing.send().await else {
                return Ok(failure);
            };
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str().into(), value.to_str().unwrap_or("").into()))
                .collect();
            let body = response.bytes().await.map_or(Vec::new(), |bytes| {
                bytes.into_iter().take(dal_provider::BODY_LIMIT).collect()
            });
            Ok(FetchResponse {
                status,
                headers,
                body,
            })
        })
    }

    fn agents(&self, op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        Box::pin(async move { Ok(self.agents_op(op).await) })
    }

    fn jobs(&self, owner: &Name, op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        let ctx = crate::jobs::JobsCtx {
            table: Arc::clone(&self.jobs),
            owner: owner.clone(),
            jobs_dir: self.jobs_dir.clone(),
            cancel: self.cancel.clone(),
        };
        Box::pin(async move { Ok(crate::jobs::run_jobs_op(ctx, op).await) })
    }

    fn scheme(
        &self,
        caller: &crate::ext::Caller,
        uri: &str,
    ) -> ServiceFuture<'_, Option<crate::ext::Doc>> {
        let Some(services) = self.services.get().cloned() else {
            return Box::pin(async {
                Err(ServiceError::failed(None, "session services are not ready"))
            });
        };
        let caller = caller.clone();
        let uri = uri.to_owned();
        let generation = self.host.shared.generation.borrow().clone();
        let session = self.session;
        let shared = Arc::clone(&self.shared);
        let initial_entries = Arc::clone(&self.initial_entries);
        let scheme_store = Arc::clone(&self.scheme_store);
        Box::pin(async move {
            let Some((scheme, _)) = uri.split_once("://") else {
                return Ok(None);
            };
            if matches!(scheme, "job" | "session") || generation.scheme(scheme).is_none() {
                return Ok(None);
            }
            let context = crate::ext::scheme::SchemeResolveContext {
                caller: &caller,
                services: &services,
                session,
            };
            let resolved = crate::session::rt::resolve_extension_scheme(
                uri.into_boxed_str(),
                &generation,
                &context,
                move || (scheme_store, shared, initial_entries),
            )
            .await;
            match resolved {
                Ok(doc) => Ok(Some(doc)),
                Err(crate::error::ToolError::Scheme(crate::error::SchemeError::NotFound {
                    ..
                })) => Ok(None),
                Err(error) => Err(ServiceError::failed(None, error.to_string())),
            }
        })
    }

    fn sidecar_artifact(
        &self,
        job: dal_core::JobId,
        file: dal_core::ArtifactFile,
        bytes: Vec<u8>,
    ) -> ServiceFuture<'_, ()> {
        let data_root = self.host.shared.data_root.clone();
        let session = self.session.to_string();
        let task = format!("{job:?}");
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                write_isolation_artifact(&data_root, &session, &task, file.file_name(), &bytes)
            })
            .await
            .map_err(|error| {
                ServiceError::failed(Some(dal_core::Service::Sidecar), error.to_string())
            })?
            .map_err(|error| {
                ServiceError::failed(Some(dal_core::Service::Sidecar), error.to_string())
            })
        })
    }

    fn turn(&self, op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        Box::pin(async move {
            match self.turn_op(op).await {
                TurnOpReply::WakeRefused(reason) => Err(wake_service_error(reason)),
                reply => Ok(reply),
            }
        })
    }

    fn append_record(&self, ext: &Name, kind: &str, body: RawValue) -> ServiceFuture<'_, EntryId> {
        let ext = ext.clone();
        let kind: Box<str> = kind.into();
        Box::pin(async move {
            let (reply, rx) = oneshot::channel();
            let request = crate::session::ExtRecordRequest {
                ext,
                kind,
                body,
                reply,
            };
            self.handle
                .ext_record(request)
                .await
                .map_err(|_| record_session_closed())?;
            rx.await.map_err(|_| record_session_closed())?
        })
    }

    fn ext_records(&self) -> Arc<[ExtRecord]> {
        self.shared.ext_records()
    }
    fn blob_put(&self, bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        Box::pin(async move {
            let id = self
                .handle
                .put_blob(bytes)
                .await
                .map_err(|error| ServiceError::failed(None, error.to_string()))?;
            Ok(*id.as_bytes())
        })
    }

    fn blob_get(&self, digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            let id = blob_id_from_digest(digest)?;
            match self.handle.blob(id).await {
                Ok(bytes) => Ok(Some(bytes)),
                Err(AgentError::BlobNotFound { .. }) => Ok(None),
                Err(error) => Err(ServiceError::failed(None, error.to_string())),
            }
        })
    }

    fn sidecar_read(&self, name: &Name) -> ServiceFuture<'_, Option<Vec<u8>>> {
        let name = name.clone();
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            let _ = self
                .handle
                .sidecar(crate::session::SidecarOp::Read { name, reply: tx })
                .await;
            Ok(rx.await.unwrap_or(None))
        })
    }

    fn sidecar_write(&self, name: &Name, bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        let name = name.clone();
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            let _ = self
                .handle
                .sidecar(crate::session::SidecarOp::Write {
                    name,
                    bytes,
                    reply: tx,
                })
                .await;
            let _ = rx.await;
            Ok(())
        })
    }

    fn infer(&self, req: ModelRequest) -> ServiceFuture<'_, Inference> {
        Box::pin(async move {
            let deps = self.infer_deps();
            let stream = super::turn::infer_stream(&deps, req, &self.cancel).await;
            crate::ext::synthetic::collect(stream)
                .await
                .map_err(|failure| crate::error::ServiceError::failed(None, failure.to_string()))
        })
    }

    fn infer_with_script(
        &self,
        req: ModelRequest,
        script: Option<Arc<crate::session::script::SessionScriptHost>>,
        cancel: CancellationToken,
    ) -> ServiceFuture<'_, Inference> {
        Box::pin(async move {
            let deps = super::turn::RequestDeps {
                session: self.session,
                host: Arc::clone(&self.host),
                script,
            };
            let session_cancel = self.cancel.clone();
            tokio::select! {
                () = session_cancel.cancelled() => Err(ServiceError::Cancelled),
                inference = async {
                    let stream = super::turn::infer_stream(&deps, req, &cancel).await;
                    crate::ext::synthetic::collect(stream)
                        .await
                        .map_err(|failure| ServiceError::failed(None, failure.to_string()))
                } => inference,
            }
        })
    }

    fn infer_stream(&self, req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        Box::pin(async move {
            Ok(super::turn::infer_stream(&self.infer_deps(), req, &self.cancel).await)
        })
    }

    fn call_tool(
        &self,
        name: &str,
        args: &crate::ext::tool::RawValue,
    ) -> ServiceFuture<'_, crate::ext::tool::ToolOutcome> {
        let name = name.to_owned();
        let args = args.clone();
        Box::pin(async move { Ok(super::dispatch::direct_call(self, &name, args).await) })
    }

    fn publish_update(&self, update: dal_core::UpdateKind) {
        self.shared.publish(update);
    }

    fn answerer_attached(&self) -> bool {
        self.shared.attached()
    }

    fn notify(&self, notice: Notice) {
        self.shared.publish(dal_core::UpdateKind::Notice(notice));
    }

    fn env(&self, key: &str) -> Option<String> {
        self.host
            .shared
            .env
            .vars
            .get(&std::ffi::OsString::from(key))
            .and_then(|value| value.to_str())
            .map(str::to_owned)
    }
}

impl Backend {
    async fn agents_op(&self, op: AgentsOp) -> AgentsReply {
        match op {
            AgentsOp::Start(start) => self.agent_start(start).await,
            AgentsOp::Await { id, timeout } => {
                if self.is_child(id) {
                    self.agent_await(id, timeout).await
                } else {
                    AgentsReply::Cancelled { id }
                }
            }
            AgentsOp::Cancel { id } => {
                if self.is_child(id) {
                    let _ = self.host().close(id).await;
                }
                AgentsReply::Cancelled { id }
            }
            AgentsOp::List => AgentsReply::Listed(self.agent_list()),
            AgentsOp::Send {
                to,
                text,
                mode,
                reply_to,
            } => self.agent_send(to, text, mode, reply_to).await,
            AgentsOp::Recv { after, timeout } => self.agent_recv(after, timeout).await,
            _ => AgentsReply::Cancelled { id: self.session },
        }
    }

    /// Returns whether `id` is a live child of this session: an agents
    /// grant may reach only the caller's own subtree, the same scope
    /// `agent_list` publishes.
    fn is_child(&self, id: SessionId) -> bool {
        self.host
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .is_some_and(|entry| entry.parent == Some(self.session))
    }

    async fn agent_start(&self, start: dal_core::AgentStart) -> AgentsReply {
        let workspace = start
            .workspace
            .clone()
            .unwrap_or_else(|| self.workspace.clone());
        // An explicit child model the catalog cannot route refuses the
        // start; silently inheriting the caller's model would run a
        // different program than the one requested.
        let model = self.resolve_child_model(start.model.as_deref()).await;
        if start.model.is_some() && model.is_none() {
            return AgentsReply::Cancelled { id: self.session };
        }
        let host = self.host();
        let Ok(child) = host
            .open(
                crate::host::SessionRef::Child {
                    parent: self.session,
                    call: start.call.clone(),
                    workspace,
                },
                dal_core::ClientId::new("core"),
            )
            .await
        else {
            return AgentsReply::Cancelled { id: self.session };
        };
        let child_id = child.inner.session;
        // The child is restricted before its first turn: a start that sets
        // `tools` runs with exactly those tools, on every turn and across
        // reloads, and an absent list leaves the child unrestricted.
        if let Some(names) = &start.tools {
            child.inner.shared.restrict_tools(names);
        }
        // The child starts under the approval mode its parent runs under now,
        // not the configured default. When the mode cannot be set the child
        // does not start.
        let approval = self.shared.approval();
        if child.inner.shared.approval() != approval
            && child
                .submit(dal_core::Command::SetApproval {
                    mode: approval,
                    save: dal_core::Save::SessionOnly,
                })
                .await
                .is_err()
        {
            let _ = self.host().close(child_id).await;
            return AgentsReply::Cancelled { id: self.session };
        }
        let mut prompt = start.prompt.to_string();
        if let Some(system) = start.system.as_ref().or(start.role.as_ref()) {
            prompt = format!("System: {system}\n\n{prompt}");
        }
        if let Some(model) = model {
            let _ = child
                .submit(dal_core::Command::SetModel {
                    model,
                    save: dal_core::Save::SessionOnly,
                })
                .await;
        }
        let _ = child
            .submit(dal_core::Command::Prompt {
                expect: dal_core::Expect::Idle,
                content: vec![Part::Text {
                    text: prompt.into(),
                }],
            })
            .await;
        AgentsReply::Started { id: child_id }
    }

    /// Resolves a child model reference; unresolvable keeps the default.
    async fn resolve_child_model(&self, reference: Option<&str>) -> Option<dal_core::ModelRoute> {
        let reference = reference?;
        if let Some(found) = crate::ext::synthetic::find(&self.host.shared, reference) {
            return Some(found.route());
        }
        let catalog = self.host.shared.providers.catalog().await.ok()?;
        let aliases: Vec<(Box<str>, Box<str>)> = self
            .host
            .shared
            .config
            .aliases()
            .iter()
            .map(|(name, target)| (name.clone(), target.clone()))
            .collect();
        dal_provider::resolve(&catalog, &aliases, reference)
            .ok()
            .map(|resolved| resolved.route)
    }

    async fn agent_await(
        &self,
        id: SessionId,
        timeout: Option<std::time::Duration>,
    ) -> AgentsReply {
        let host = self.host();
        let Ok(agent) = host
            .open(
                crate::host::SessionRef::Resume {
                    key: id.to_string().into(),
                    workspace: self.workspace.clone(),
                },
                dal_core::ClientId::new("core"),
            )
            .await
        else {
            return AgentsReply::Cancelled { id };
        };
        let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
        loop {
            if self.cancel.is_cancelled() {
                return AgentsReply::Cancelled { id };
            }
            let Ok(view) = agent.view(dal_core::PageReq::default()) else {
                return AgentsReply::Cancelled { id };
            };
            if matches!(view.turn, dal_core::TurnState::Idle) {
                let report = Self::child_report(id, &view);
                if matches!(report, AgentsReply::Await { .. })
                    && let Some(entry) = self
                        .host
                        .sessions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(&id)
                {
                    entry
                        .reported
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                return report;
            }
            if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                return AgentsReply::Pending { id };
            }
            match deadline {
                Some(deadline) => {
                    tokio::select! {
                        biased;
                        () = self.cancel.cancelled() => return AgentsReply::Cancelled { id },
                        () = tokio::time::sleep_until(deadline) => return AgentsReply::Pending { id },
                        () = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
                    }
                }
                None => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
            }
        }
    }

    /// Builds the completion report from the child's last assistant entry.
    fn child_report(id: SessionId, view: &dal_core::View) -> AgentsReply {
        let mut text = String::new();
        let mut entry = view.entries.items.last().map(|item| item.id);
        for item in view.entries.items.iter().rev() {
            if let dal_core::EntryKind::Assistant { content, .. } = &item.kind {
                for block in content.iter().rev() {
                    if let dal_core::Block::Text { text: chunk } = block {
                        text.insert_str(0, chunk);
                    }
                }
                entry = Some(item.id);
                break;
            }
        }
        // An entryless child has no report pointer; MIN marks the absence.
        let entry = entry.unwrap_or_else(|| dal_core::EntryId::new(std::num::NonZeroU64::MIN));
        AgentsReply::Await {
            report: AgentReport {
                stop: dal_core::Stop::EndTurn,
                text: text.into(),
                session: id,
                entry,
            },
        }
    }

    fn agent_list(&self) -> Vec<AgentInfo> {
        let sessions = self
            .host
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions
            .iter()
            // Only the caller's own children: an unscoped list lets one
            // session's session-end sweep cancel siblings' subtrees, which
            // compounds to O(n²) closes across a shutdown cascade.
            .filter(|(_, entry)| entry.parent == Some(self.session))
            .map(|(id, entry)| {
                let name = entry
                    .shared
                    .snapshot(crate::session::projection::SnapshotArgs {
                        generation: entry.generation,
                        id: *id,
                        workspace: entry.workspace.clone(),
                        open: Vec::new(),
                        updated_at: dal_core::Timestamp::now(),
                        created_at: None,
                        archived: None,
                        page: dal_core::PageReq::default(),
                    })
                    .session
                    .name
                    .unwrap_or_default();
                AgentInfo {
                    id: *id,
                    name,
                    state: AgentState::Running,
                }
            })
            .collect()
    }

    async fn agent_send(
        &self,
        to: SessionId,
        text: Box<str>,
        mode: MailMode,
        reply_to: Option<EntryId>,
    ) -> AgentsReply {
        let mail = ExtMail {
            from: self.session,
            to,
            mode,
            text,
            reply_to,
        };
        let handle = {
            let sessions = self
                .host
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let sender_root = session_root(&sessions, self.session);
            let recipient_root = session_root(&sessions, to);
            if sender_root.is_none() || sender_root != recipient_root {
                return AgentsReply::Delivered(dal_core::ext::Receipt::Gone);
            }
            sessions
                .get(&to)
                .filter(|entry| !entry.reported.load(std::sync::atomic::Ordering::SeqCst))
                .map(|entry| entry.handle.clone())
        };
        let Some(handle) = handle else {
            return AgentsReply::Delivered(dal_core::ext::Receipt::Gone);
        };
        let (tx, rx) = oneshot::channel();
        let outcome = handle
            .mail(crate::session::MailRequest::Send { mail, reply: tx })
            .await;
        match (outcome, rx.await) {
            (Ok(()), Ok(Some(receipt))) => AgentsReply::Delivered(receipt),
            _ => AgentsReply::Delivered(dal_core::ext::Receipt::Gone),
        }
    }

    async fn agent_recv(
        &self,
        after: Option<EntryId>,
        timeout: Option<std::time::Duration>,
    ) -> AgentsReply {
        let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
        loop {
            let (reply, rx) = oneshot::channel();
            if self
                .handle
                .mail(crate::session::MailRequest::Recv { after, reply })
                .await
                .is_err()
            {
                return AgentsReply::Received {
                    mail: Vec::new(),
                    next: after,
                };
            }
            let Ok((mail, next)) = rx.await else {
                return AgentsReply::Received {
                    mail: Vec::new(),
                    next: after,
                };
            };
            if !mail.is_empty() || deadline.is_none() || timeout.is_some_and(|d| d.is_zero()) {
                return AgentsReply::Received { mail, next };
            }
            let Some(deadline) = deadline else {
                return AgentsReply::Received { mail, next };
            };
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => return AgentsReply::Received { mail, next },
                () = tokio::time::sleep_until(deadline) => return AgentsReply::Received { mail, next },
                () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
            }
        }
    }

    async fn turn_op(&self, op: TurnOp) -> TurnOpReply {
        let (tx, rx) = oneshot::channel();
        let outcome = self
            .handle
            .turn(crate::session::TurnRequest { op, reply: tx })
            .await;
        if outcome.is_err() {
            return TurnOpReply::Idle(true);
        }
        rx.await.unwrap_or(TurnOpReply::Idle(true))
    }

    fn infer_deps(&self) -> super::turn::RequestDeps {
        super::turn::RequestDeps {
            session: self.session,
            host: Arc::clone(&self.host),
            script: None,
        }
    }
}
