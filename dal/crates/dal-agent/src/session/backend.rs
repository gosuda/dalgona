//! Session data-plane behind extension services.
//!
//! The backend performs ungated file, network, agent, job, turn, sidecar,
//! inference, tool, notice, and env operations; all capability gates live
//! in [`SessionServices`]. It also backs [`ToolCxRuntime`]: approval ladder,
//! process launch, job parking, scheme resolution, and workspace access.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use dal_core::ext::{Mail as ExtMail, Service, SidecarName};
use dal_core::{
    AgentInfo, AgentRefusal, AgentReport, AgentState, AgentsOp, AgentsReply, ApprovalMode, BlobId,
    Command, EntryId, Expect, FetchMethod, FetchRequest, FetchResponse, Inference, JobsOp,
    JobsReply, MailMode, ModelRequest, Name, Notice, Part, RawJson, Reply, Request, SessionId,
    StateError, StateOp, StateRecord, TurnOp, TurnOpReply, Workspace,
};
use dal_provider::EventStream;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::broker::Broker;
use crate::error::{AgentError, DenyReason, HostError, ServiceError};
use crate::ext::ExtRecord;
use crate::ext::services::{ServiceFuture, SessionBackend, SessionServices};
use crate::ext::tool::RawValue;
use crate::host::HostState;
use crate::session::shared::Shared;
use crate::session::tasks::SessionTasks;
use crate::session::{ExtRecordRequest, SessionHandle};

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

/// Encodes the durable start policy of one child session: its tool
/// allowlist and the approval mode it inherits from its parent.
fn child_policy_body(
    tools: Option<&[Name]>,
    approval: ApprovalMode,
) -> Result<RawJson, ServiceError> {
    #[derive(serde::Serialize)]
    struct Policy<'a> {
        tools: Option<Vec<&'a str>>,
        approval: ApprovalMode,
    }
    let tools = tools.map(|names| names.iter().map(Name::as_str).collect());
    let body = sonic_rs::to_string(&Policy { tools, approval })
        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
    RawJson::parse(&body).map_err(|error| ServiceError::failed(None, error.to_string()))
}

/// Moves the child onto the parent's approval mode; a no-op when they
/// already agree.
async fn copy_child_approval(
    child: &crate::agent::Agent,
    approval: ApprovalMode,
) -> Result<(), AgentError> {
    if child.inner.shared.approval() == approval {
        return Ok(());
    }
    child
        .submit(dal_core::Command::SetApproval {
            mode: approval,
            save: dal_core::Save::SessionOnly,
        })
        .await
        .map(|_| ())
}

/// Runs the prompted turn's interrupt inside the child's task group: after
/// `delay` the turn is cancelled, and the timer dies with the session.
fn spawn_prompt_interrupt(tasks: &SessionTasks, handle: SessionHandle, delay: std::time::Duration) {
    tasks.spawn(async move {
        tokio::time::sleep(delay).await;
        let (reply, receipt) = oneshot::channel();
        if handle
            .turn(crate::session::TurnRequest {
                op: TurnOp::Cancel,
                reply,
            })
            .await
            .is_ok()
        {
            let _ = receipt.await;
        }
    });
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

/// A workspace path and its resolved location at the time of the check.
#[must_use]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Confined {
    relative: PathBuf,
    resolved: PathBuf,
}

impl Confined {
    /// The canonical location below the canonical root. Components that do
    /// not exist yet are appended as written.
    #[must_use]
    pub(crate) fn resolved(&self) -> &Path {
        &self.resolved
    }

    /// Resolves the same relative path again and proves it still names the
    /// same location inside the root.
    ///
    /// # Errors
    ///
    /// Returns the refusal of [`confine`] when the path now leaves the root,
    /// and [`ConfineError::Changed`] when it now resolves elsewhere inside
    /// the root, such as through a link created since the first resolution.
    pub(crate) fn recheck(&self, root: &Path) -> Result<(), ConfineError> {
        if confine(root, &self.relative)?.resolved == self.resolved {
            Ok(())
        } else {
            Err(ConfineError::Changed)
        }
    }
}

/// Why a path is not contained in a workspace root.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ConfineError {
    /// The path is empty.
    #[error("the path is empty")]
    Empty,
    /// The path is absolute or carries a platform prefix.
    #[error("the path is absolute")]
    Absolute,
    /// The path climbs with a `..` component.
    #[error("the path climbs out of its directory with `..`")]
    ParentDir,
    /// The path resolves outside the root.
    #[error("the path resolves outside the workspace")]
    Outside,
    /// The root or a component of the path exists but cannot be resolved,
    /// such as a dangling link.
    #[error("the path cannot be resolved")]
    Unresolvable,
    /// The path resolves to a different location than it did before.
    #[error("the path now resolves to a different location")]
    Changed,
}

/// Proves that `raw` names a location inside `root`.
///
/// # Errors
///
/// Returns [`ConfineError`] when `raw` is empty, absolute, carries a
/// platform prefix, climbs with `..`, resolves outside the root, or runs
/// through a component that exists but cannot be resolved.
pub(crate) fn confine(root: &Path, raw: &Path) -> Result<Confined, ConfineError> {
    if raw.as_os_str().is_empty() {
        return Err(ConfineError::Empty);
    }
    let mut relative = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => return Err(ConfineError::Absolute),
            Component::ParentDir => return Err(ConfineError::ParentDir),
            Component::CurDir => {}
            Component::Normal(part) => relative.push(part),
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(ConfineError::Empty);
    }
    let canonical_root = std::fs::canonicalize(root).map_err(|_| ConfineError::Unresolvable)?;
    let resolved = canonicalize_existing_prefix(&canonical_root.join(&relative))
        .ok_or(ConfineError::Unresolvable)?;
    if !resolved.starts_with(&canonical_root) {
        return Err(ConfineError::Outside);
    }
    Ok(Confined { relative, resolved })
}

/// Canonicalizes the deepest existing ancestor and re-appends the missing
/// tail. Returns `None` when a component exists but cannot be resolved.
///
/// This resolves the path at the time of the call. It does not prevent
/// concurrent directory replacement before a later filesystem operation.
#[must_use]
pub fn canonicalize_existing_prefix(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = path;
    loop {
        match std::fs::canonicalize(current) {
            Ok(mut resolved) => {
                resolved.extend(tail.iter().rev());
                return Some(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(current).is_ok() {
                    return None;
                }
                tail.push(current.file_name()?);
                current = current.parent()?;
            }
            Err(_) => return None,
        }
    }
}

/// Resolves a script-supplied path inside the session root on a blocking
/// thread.
async fn confine_path(
    root: PathBuf,
    path: &str,
    service: Service,
) -> Result<Confined, ServiceError> {
    let raw = PathBuf::from(path);
    let outcome = tokio::task::spawn_blocking(move || confine(&root, &raw))
        .await
        .map_err(|error| ServiceError::failed(Some(service), error.to_string()))?;
    outcome.map_err(|error| refusal(service, path, error))
}

/// Proves on a blocking thread that a staged path still resolves to the same
/// location inside the session root.
async fn recheck_path(
    root: PathBuf,
    confined: Confined,
    path: &str,
    service: Service,
) -> Result<Confined, ServiceError> {
    let (confined, outcome) = tokio::task::spawn_blocking(move || {
        let outcome = confined.recheck(&root);
        (confined, outcome)
    })
    .await
    .map_err(|error| ServiceError::failed(Some(service), error.to_string()))?;
    outcome
        .map(|()| confined)
        .map_err(|error| refusal(service, path, error))
}

fn refusal(service: Service, path: &str, error: ConfineError) -> ServiceError {
    match error {
        ConfineError::Unresolvable => {
            ServiceError::failed(Some(service), format!("{service} \"{path}\": {error}"))
        }
        _ => ServiceError::Denied(DenyReason::out_of_scope(format!("path \"{path}\""))),
    }
}

fn fs_failure(service: Service, path: &str, error: &std::io::Error) -> ServiceError {
    ServiceError::failed(
        Some(service),
        format!("{service} \"{path}\" failed: {error}"),
    )
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
            if entry.shared.attached_approval() {
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

    fn host(&self) -> crate::host::Host {
        crate::host::Host {
            state: Arc::clone(&self.host),
        }
    }
}

impl SessionBackend for Backend {
    fn fs_read(&self, path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        let root = self.canonical_root.clone();
        let path = path.to_owned();
        Box::pin(async move {
            let confined = confine_path(root, &path, Service::FsRead).await?;
            match tokio::fs::read(confined.resolved()).await {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(fs_failure(Service::FsRead, &path, &error)),
            }
        })
    }

    fn fs_write(&self, path: &str, bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        let root = self.canonical_root.clone();
        let path = path.to_owned();
        Box::pin(async move {
            let service = Service::FsWrite;
            let confined = confine_path(root.clone(), &path, service).await?;
            if let Some(parent) = confined.resolved().parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| fs_failure(service, &path, &error))?;
            }
            let confined = recheck_path(root, confined, &path, service).await?;
            tokio::fs::write(confined.resolved(), bytes)
                .await
                .map_err(|error| fs_failure(service, &path, &error))
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
            let Ok(mut response) = outgoing.send().await else {
                return Ok(failure);
            };
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str().into(), value.to_str().unwrap_or("").into()))
                .collect();
            let mut body = Vec::new();
            while body.len() < dal_provider::BODY_LIMIT
                && let Ok(Some(chunk)) = response.chunk().await
            {
                let room = dal_provider::BODY_LIMIT - body.len();
                body.extend_from_slice(&chunk[..chunk.len().min(room)]);
            }
            Ok(FetchResponse {
                status,
                headers,
                body,
            })
        })
    }

    fn agents(&self, op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        Box::pin(async move { self.agents_op(op).await })
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
        let task = job.to_string();
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
    fn request_opened(&self, request: Request) -> ServiceFuture<'_, ()> {
        let handle = self.handle.clone();
        Box::pin(async move {
            handle
                .work(crate::session::actor::TurnWork::Asked { request })
                .await
                .map_err(|error| ServiceError::failed(None, error.to_string()))
        })
    }

    fn request_resolved(&self, resolved: crate::broker::Resolved) {
        self.handle
            .work_detached(crate::session::actor::TurnWork::Answered { resolved });
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

    fn sidecar_read(&self, ext: &Name, name: &SidecarName) -> ServiceFuture<'_, Option<Vec<u8>>> {
        let ext = ext.clone();
        let name = name.clone();
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            self.handle
                .sidecar(crate::session::SidecarOp::Read {
                    ext,
                    name,
                    reply: tx,
                })
                .await
                .map_err(|error| ServiceError::failed(None, error.to_string()))?;
            let result = rx.await.map_err(|_| record_session_closed())?;
            result.map_err(|error| ServiceError::failed(Some(Service::Sidecar), error))
        })
    }

    fn sidecar_write(
        &self,
        ext: &Name,
        name: &SidecarName,
        bytes: Vec<u8>,
    ) -> ServiceFuture<'_, ()> {
        let ext = ext.clone();
        let name = name.clone();
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            self.handle
                .sidecar(crate::session::SidecarOp::Write {
                    ext,
                    name,
                    bytes,
                    reply: tx,
                })
                .await
                .map_err(|error| ServiceError::failed(None, error.to_string()))?;
            let result = rx.await.map_err(|_| record_session_closed())?;
            result.map_err(|error| ServiceError::failed(Some(Service::Sidecar), error))
        })
    }

    fn state(&self, op: StateOp) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            let _ = self
                .handle
                .state(crate::session::StateReq { op, reply: tx })
                .await;
            Ok(rx.await.unwrap_or(Err(StateError::Unavailable)))
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
        self.shared.attached_ask()
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

fn cancel_child(id: SessionId, result: Result<(), HostError>) -> Result<AgentsReply, ServiceError> {
    result
        .map(|()| AgentsReply::Cancelled { id })
        .map_err(|error| {
            ServiceError::failed(
                Some(Service::Agents),
                format!("could not cancel child session {id}: {error}"),
            )
        })
}

fn refused(reason: AgentRefusal) -> AgentsReply {
    AgentsReply::Refused { reason }
}

fn child_start_error(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::failed(
        Some(Service::Agents),
        format!("could not start child session: {error}"),
    )
}

impl Backend {
    async fn agents_op(&self, op: AgentsOp) -> Result<AgentsReply, ServiceError> {
        match op {
            AgentsOp::Start(start) => self.agent_start(start).await,
            AgentsOp::Prompt {
                id,
                text,
                interrupt,
                max_steps,
            } => Ok(self.agent_prompt(id, text, interrupt, max_steps).await),
            AgentsOp::Await { id, timeout } => {
                if self.is_child(id) {
                    Ok(self.agent_await(id, timeout).await)
                } else {
                    Ok(AgentsReply::Cancelled { id })
                }
            }
            AgentsOp::Cancel { id } => {
                if self.is_child(id) {
                    cancel_child(id, self.host().close(id).await)
                } else {
                    Ok(AgentsReply::Cancelled { id })
                }
            }
            AgentsOp::List => Ok(AgentsReply::Listed(self.agent_list())),
            AgentsOp::Send {
                to,
                text,
                mode,
                reply_to,
            } => Ok(self.agent_send(to, text, mode, reply_to).await),
            AgentsOp::Recv { after, timeout } => Ok(self.agent_recv(after, timeout).await),
            _ => Ok(AgentsReply::Cancelled { id: self.session }),
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

    /// The depth limit when this session may not start a child: a child
    /// of this session would sit one level deeper than `agents.max_depth`.
    fn depth_refusal(&self) -> Option<u32> {
        let max = self.host.shared.config.agents().max_depth.get();
        let depth = self
            .host
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&self.session)
            .map_or(0, |entry| entry.depth);
        (depth + 1 > max).then_some(max)
    }

    /// Resolves the child workspace and keeps it inside the caller's root:
    /// an absolute path outside it would widen the `agents` grant into
    /// tool access across the whole filesystem. The lexical spelling
    /// lies — `/root/../etc` starts with `/root` — so containment is
    /// checked on canonical paths; a workspace that cannot be resolved
    /// fails closed. The checked canonical path is the one the child
    /// receives: re-resolving the lexical spelling later would race a
    /// swapped symlink into an outside root. The containment root is the
    /// one `Backend::new` captured, so a replaceable symlink at the
    /// session workspace cannot shift the boundary mid-session.
    fn child_workspace(&self, requested: Option<&Workspace>) -> Result<Workspace, AgentRefusal> {
        let workspace = requested.unwrap_or(&self.workspace);
        let child_root = std::fs::canonicalize(workspace.as_path())
            .map_err(|_| AgentRefusal::WorkspaceUnresolved)?;
        if !child_root.starts_with(&self.canonical_root) {
            return Err(AgentRefusal::WorkspaceOutsideRoot);
        }
        Workspace::new(child_root).map_err(|_| AgentRefusal::WorkspaceUnresolved)
    }

    async fn agent_start(&self, start: dal_core::AgentStart) -> Result<AgentsReply, ServiceError> {
        if let Some(max_depth) = self.depth_refusal() {
            return Ok(refused(AgentRefusal::MaxDepth { max_depth }));
        }
        let workspace = match self.child_workspace(start.workspace.as_ref()) {
            Ok(workspace) => workspace,
            Err(reason) => return Ok(refused(reason)),
        };
        // An explicit child model the catalog cannot route refuses the
        // start; silently inheriting the caller's model would run a
        // different program than the one requested.
        let model = self.resolve_child_model(start.model.as_deref()).await;
        if let (Some(requested), None) = (start.model.as_deref(), &model) {
            return Ok(refused(AgentRefusal::ModelUnroutable {
                model: requested.into(),
            }));
        }
        let host = self.host();
        let child = host
            .open(
                crate::host::SessionRef::Child {
                    parent: self.session,
                    call: start.call.clone(),
                    workspace,
                    // Queued for the first durable record: a start cancelled
                    // before it records leaves no named session behind.
                    name: Some(start.name.clone()),
                },
                dal_core::ClientId::new("core"),
            )
            .await
            .map_err(child_start_error)?;
        let child_id = child.inner.session;
        // The child is restricted before its first turn: a start that sets
        // `tools` runs with exactly those tools, on every turn and across
        // reloads, and an absent list leaves the child unrestricted.
        // The restriction and the inherited approval mode are journaled
        // through the child's actor first: a start whose policy cannot be
        // recorded leaves no half-restricted child behind.
        let Ok(body) = child_policy_body(start.tools.as_deref(), self.shared.approval()) else {
            let _ = self.host().close(child_id).await;
            return Err(child_start_error("could not encode the child start policy"));
        };
        let Ok(ext) = Name::parse(crate::host::CHILD_POLICY_EXT) else {
            let _ = self.host().close(child_id).await;
            return Err(child_start_error("could not encode the child start policy"));
        };
        let (reply, receipt) = oneshot::channel();
        let queued = child
            .inner
            .handle
            .ext_record(ExtRecordRequest {
                ext,
                kind: crate::host::CHILD_POLICY_KIND.into(),
                body,
                reply,
            })
            .await
            .is_ok();
        let persisted = queued && receipt.await.is_ok_and(|result| result.is_ok());
        if !persisted {
            let _ = self.host().close(child_id).await;
            return Err(child_start_error(
                "could not journal the child start policy",
            ));
        }
        // Keep the live snapshot in step with the durable policy before the
        // first prompt enters the child.
        if let Some(names) = &start.tools {
            child.inner.shared.restrict_tools(names);
        }
        // The child starts under the approval mode its parent runs under now,
        // not the configured default. When the mode cannot be set the child
        // does not start.
        let approval = self.shared.approval();
        if let Err(error) = copy_child_approval(&child, approval).await {
            let _ = self.host().close(child_id).await;
            return Err(child_start_error(error));
        }
        let mut prompt = start.prompt.to_string();
        if let Some(system) = start.system.as_ref().or(start.role.as_ref()) {
            prompt = format!("System: {system}\n\n{prompt}");
        }
        if let Some(model) = model
            && let Err(error) = child
                .submit(dal_core::Command::SetModel {
                    model,
                    save: dal_core::Save::SessionOnly,
                })
                .await
        {
            let _ = host.close(child_id).await;
            return Err(child_start_error(error));
        }
        if let Err(error) = child
            .submit(dal_core::Command::Prompt {
                expect: dal_core::Expect::Idle,
                content: vec![Part::Text {
                    text: prompt.into(),
                }],
            })
            .await
        {
            let _ = host.close(child_id).await;
            return Err(child_start_error(error));
        }
        Ok(AgentsReply::Started { id: child_id })
    }

    /// Starts one prompt turn on an idle child of this session and
    /// optionally interrupts it after `interrupt`. A child takes one
    /// prompt this way: the slot is claimed before the turn starts and
    /// released only when the child refuses it.
    async fn agent_prompt(
        &self,
        id: SessionId,
        text: Box<str>,
        interrupt: Option<std::time::Duration>,
        max_steps: Option<std::num::NonZeroU32>,
    ) -> AgentsReply {
        let Some((handle, child_tasks, child_shared)) = self.claim_prompt_slot(id) else {
            return AgentsReply::Cancelled { id };
        };
        // The bound belongs to the one turn this prompt starts: the turn
        // takes it when it begins, and a refused prompt clears it.
        child_shared.set_next_turn_step_cap(max_steps);
        let reply = handle
            .submit(
                Command::Prompt {
                    expect: Expect::Idle,
                    content: vec![Part::Text { text }],
                },
                dal_core::ClientId::new("core"),
            )
            .await;
        if !matches!(reply, Ok(Reply::Accepted { .. })) {
            child_shared.set_next_turn_step_cap(None);
            self.release_prompt_slot(id);
            return AgentsReply::Cancelled { id };
        }
        if let Some(delay) = interrupt {
            spawn_prompt_interrupt(&child_tasks, handle, delay);
        }
        AgentsReply::Prompted { id }
    }

    /// Claims the child's single prompt slot: only the caller's own live
    /// child, and only the first time.
    fn claim_prompt_slot(
        &self,
        id: SessionId,
    ) -> Option<(SessionHandle, SessionTasks, Arc<Shared>)> {
        let sessions = self
            .host
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sessions
            .get(&id)
            .filter(|entry| entry.parent == Some(self.session))?;
        entry
            .prompted
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
            .then(|| {
                (
                    entry.handle.clone(),
                    entry.tasks.clone(),
                    Arc::clone(&entry.shared),
                )
            })
    }

    /// Gives back the prompt slot after the child refused the prompt.
    fn release_prompt_slot(&self, id: SessionId) {
        if let Some(entry) = self
            .host
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
        {
            entry
                .prompted
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
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
                let stop = self
                    .host
                    .sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&id)
                    .and_then(|entry| entry.shared.last_stop())
                    .unwrap_or(dal_core::Stop::EndTurn);
                let report = Self::child_report(id, &view, stop);
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
    /// `stop` is the child's durable terminal stop, read off its projection;
    /// `view.turn` alone only knows the turn ended, not why. An explicit
    /// `report` tool call on any assistant entry wins over the last text.
    fn child_report(id: SessionId, view: &dal_core::View, stop: dal_core::Stop) -> AgentsReply {
        #[derive(serde::Deserialize)]
        struct ReportInput {
            report: Box<str>,
        }

        let mut text = String::new();
        let mut reported_text = None;
        let mut entry = view.entries.items.last().map(|item| item.id);
        let mut latest_assistant = true;
        for item in view.entries.items.iter().rev() {
            let dal_core::EntryKind::Assistant { content, .. } = &item.kind else {
                continue;
            };
            if latest_assistant {
                for block in content.iter().rev() {
                    if let dal_core::Block::Text { text: chunk } = block {
                        text.insert_str(0, chunk);
                    }
                }
                entry = Some(item.id);
                latest_assistant = false;
            }
            reported_text = content.iter().rev().find_map(|block| {
                let dal_core::Block::ToolCall { name, input, .. } = block else {
                    return None;
                };
                (name.as_ref() == "report")
                    .then_some(input)
                    .and_then(|input| sonic_rs::from_str::<ReportInput>(input.as_str()).ok())
                    .map(|fields| fields.report)
            });
            if reported_text.is_some() {
                break;
            }
        }
        // An entryless child has no report pointer; MIN marks the absence.
        let entry = entry.unwrap_or_else(|| dal_core::EntryId::new(std::num::NonZeroU64::MIN));
        AgentsReply::Await {
            report: AgentReport {
                stop,
                text: reported_text.unwrap_or_else(|| text.into_boxed_str()),
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
                let view = entry
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
                    });
                let state = match view.turn {
                    // `Idle` only means no turn is running; the terminal
                    // reason survives on the projection's last-stop record.
                    dal_core::TurnState::Idle => AgentState::Done(
                        entry.shared.last_stop().unwrap_or(dal_core::Stop::EndTurn),
                    ),
                    _ => AgentState::Running,
                };
                AgentInfo {
                    id: *id,
                    name: view.session.name.unwrap_or_default(),
                    state,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{Env, Host, Product, SessionRef};
    use dal_core::{ApprovalMode, CallId, ClientId, Config, ConfigProduct, Workspace};

    #[test]
    fn cancel_child_reports_non_lifecycle_close_failures() {
        let id = SessionId::new_v7();
        let error = HostError::Config {
            message: "close refused".into(),
        };
        let result = cancel_child(id, Err(error)).expect_err("close failure must propagate");
        assert_eq!(
            result.to_string(),
            format!("could not cancel child session {id}: close refused")
        );
    }

    #[tokio::test]
    async fn closed_child_refuses_approval_copy() {
        let temp = tempfile::tempdir().expect("temporary data root");
        let data = temp.path().join("data");
        let workspace_path = temp.path().join("workspace");
        std::fs::create_dir_all(&data).expect("data root");
        std::fs::create_dir_all(&workspace_path).expect("workspace");
        let config = Config::load(ConfigProduct::Dalgon, &data, "", None).expect("config");
        let workspace = Workspace::new(workspace_path.clone()).expect("workspace value");
        let env = Env::data_root(workspace_path);
        let host = Host::start(
            Product {
                name: "dal",
                data_root: data,
                defaults: "",
                extensions: Vec::new(),
                bundled: Vec::new(),
            },
            config,
            env,
        )
        .await
        .expect("host");
        let root = host
            .open(
                SessionRef::New {
                    workspace: workspace.clone(),
                    name: None,
                },
                ClientId::new("backend-test"),
            )
            .await
            .expect("root");
        let child = host
            .open(
                SessionRef::Child {
                    parent: root.inner.session,
                    call: CallId::new("approval-copy"),
                    workspace,
                    name: None,
                },
                ClientId::new("backend-test"),
            )
            .await
            .expect("child");
        host.close(child.inner.session).await.expect("close child");
        assert!(
            copy_child_approval(&child, ApprovalMode::All)
                .await
                .is_err(),
            "a closed child must refuse the approval copy"
        );
        host.close(root.inner.session).await.expect("close root");
    }

    /// A real host with one root session and one child of it.
    async fn host_with_child() -> (Host, SessionId, SessionId, tempfile::TempDir) {
        let temp = tempfile::tempdir().expect("temporary data root");
        let data = temp.path().join("data");
        let workspace_path = temp.path().join("workspace");
        std::fs::create_dir_all(&data).expect("data root");
        std::fs::create_dir_all(&workspace_path).expect("workspace");
        let config = Config::load(ConfigProduct::Dalgon, &data, "", None).expect("config");
        let workspace = Workspace::new(workspace_path.clone()).expect("workspace value");
        let host = Host::start(
            Product {
                name: "dal",
                data_root: data,
                defaults: "",
                extensions: Vec::new(),
                bundled: Vec::new(),
            },
            config,
            Env::data_root(workspace_path),
        )
        .await
        .expect("host");
        let root = host
            .open(
                SessionRef::New {
                    workspace: workspace.clone(),
                    name: None,
                },
                ClientId::new("backend-test"),
            )
            .await
            .expect("root");
        let root_id = root.inner.session;
        let child = host
            .open(
                SessionRef::Child {
                    parent: root_id,
                    call: CallId::new("refusal"),
                    workspace,
                    name: None,
                },
                ClientId::new("backend-test"),
            )
            .await
            .expect("child");
        (host, root_id, child.inner.session, temp)
    }

    fn backend_of(host: &Host, id: SessionId) -> Arc<Backend> {
        let sessions = host
            .state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(&sessions.get(&id).expect("live session").backend)
    }

    #[tokio::test]
    async fn fs_services_confine_paths_and_report_failures() {
        let (host, root, child, temp) = host_with_child().await;
        let backend = backend_of(&host, root);
        let workspace = temp.path().join("workspace");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).expect("outside directory");

        for path in ["new/../../outside/file", "../outside/file"] {
            let result = backend.fs_write(path, b"escape".to_vec()).await;
            assert!(result.is_err(), "{path} must be refused");
        }
        let absolute = outside.join("absolute.txt");
        let result = backend
            .fs_write(absolute.to_string_lossy().as_ref(), b"escape".to_vec())
            .await;
        assert!(result.is_err(), "absolute path must be refused");
        assert!(!outside.join("file").exists());
        assert!(!outside.join("absolute.txt").exists());

        backend
            .fs_write("nested/path/file.txt", b"inside".to_vec())
            .await
            .expect("nested write");
        assert_eq!(
            backend.fs_read("nested/path/file.txt").await.expect("read"),
            Some(b"inside".to_vec())
        );

        std::fs::create_dir(workspace.join("directory")).expect("directory");
        assert!(backend.fs_read("directory").await.is_err());
        assert_eq!(
            backend.fs_read("missing").await.expect("missing read"),
            None
        );

        host.close(child).await.expect("close child");
        host.close(root).await.expect("close session");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_write_refuses_a_symlink_swap_before_directory_creation() {
        let (host, root, child, temp) = host_with_child().await;
        let backend = backend_of(&host, root);
        let workspace = temp.path().join("workspace");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).expect("outside directory");
        std::os::unix::fs::symlink(&outside, workspace.join("link")).expect("outside link");
        assert!(
            backend
                .fs_write("link/file.txt", b"escape".to_vec())
                .await
                .is_err()
        );
        assert!(!outside.join("file.txt").exists());

        let future = backend.fs_write("staged/file.txt", b"escape".to_vec());
        std::os::unix::fs::symlink(&outside, workspace.join("staged")).expect("swap symlink");
        assert!(future.await.is_err());
        assert!(!outside.join("staged").exists());

        host.close(child).await.expect("close child");
        host.close(root).await.expect("close session");
    }

    fn start_in(workspace: Option<Workspace>, model: Option<&str>) -> dal_core::AgentStart {
        dal_core::AgentStart {
            call: CallId::new("refused-start"),
            name: "member".into(),
            prompt: "work".into(),
            model: model.map(Into::into),
            role: None,
            system: None,
            tools: None,
            workspace,
        }
    }

    #[tokio::test]
    async fn a_start_past_max_depth_reaches_the_parent_with_its_reason() {
        let (host, _root, child, _temp) = host_with_child().await;
        let reply = backend_of(&host, child)
            .agents_op(AgentsOp::Start(start_in(None, None)))
            .await
            .expect("a refusal is a reply");
        let AgentsReply::Refused { reason } = reply else {
            panic!("expected a refusal, got {reply:?}");
        };
        assert_eq!(reason, AgentRefusal::MaxDepth { max_depth: 1 });
        assert_eq!(
            reason.to_string(),
            "child sessions cannot start children here: agents.max_depth = 1."
        );
    }

    #[tokio::test]
    async fn workspace_refusals_name_the_reason() {
        let (host, root, _child, temp) = host_with_child().await;
        let backend = backend_of(&host, root);
        let outside = Workspace::new(temp.path().to_path_buf()).expect("outside workspace");
        let missing =
            Workspace::new(temp.path().join("workspace").join("absent")).expect("absent workspace");
        for (workspace, expected) in [
            (outside, AgentRefusal::WorkspaceOutsideRoot),
            (missing, AgentRefusal::WorkspaceUnresolved),
        ] {
            let reply = backend
                .agents_op(AgentsOp::Start(start_in(Some(workspace), None)))
                .await
                .expect("a refusal is a reply");
            assert_eq!(reply, AgentsReply::Refused { reason: expected });
        }
    }

    #[tokio::test]
    async fn an_unroutable_model_refuses_the_start_with_its_name() {
        let (host, root, _child, _temp) = host_with_child().await;
        let reply = backend_of(&host, root)
            .agents_op(AgentsOp::Start(start_in(None, Some("acme/none"))))
            .await
            .expect("a refusal is a reply");
        assert_eq!(
            reply,
            AgentsReply::Refused {
                reason: AgentRefusal::ModelUnroutable {
                    model: "acme/none".into()
                }
            }
        );
    }

    #[tokio::test]
    async fn a_cancelled_child_still_reports_cancelled() {
        let (host, root, child, _temp) = host_with_child().await;
        let reply = backend_of(&host, root)
            .agents_op(AgentsOp::Cancel { id: child })
            .await
            .expect("cancel is a reply");
        assert_eq!(reply, AgentsReply::Cancelled { id: child });
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixture() -> Result<(tempfile::TempDir, PathBuf, PathBuf), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root)?;
        std::fs::create_dir(&outside)?;
        Ok((temp, root, outside))
    }

    #[test]
    fn lexical_refusals_precede_any_filesystem_access() {
        let missing = Path::new("/nonexistent-dal-confine-root");
        for (raw, expected) in [
            ("", ConfineError::Empty),
            ("/etc/passwd", ConfineError::Absolute),
            ("../x", ConfineError::ParentDir),
            ("a/../b", ConfineError::ParentDir),
            ("new/../../outside/file", ConfineError::ParentDir),
        ] {
            assert_eq!(confine(missing, Path::new(raw)), Err(expected), "{raw}");
        }
    }

    #[test]
    fn a_missing_tail_resolves_below_the_canonical_root() -> TestResult {
        let (_temp, root, _outside) = fixture()?;
        let confined = confine(&root, Path::new("./new/deep/file.txt"))?;
        assert_eq!(
            confined.resolved(),
            std::fs::canonicalize(&root)?.join("new/deep/file.txt")
        );
        Ok(())
    }

    #[test]
    fn a_file_cannot_be_used_as_a_parent_directory() -> TestResult {
        let (_temp, root, _outside) = fixture()?;
        std::fs::write(root.join("file"), b"existing content")?;
        assert_eq!(
            confine(&root, Path::new("file/child")),
            Err(ConfineError::Unresolvable)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn links_out_of_the_root_are_refused_through_missing_tails() -> TestResult {
        let (_temp, root, outside) = fixture()?;
        std::os::unix::fs::symlink(&outside, root.join("link"))?;
        assert_eq!(
            confine(&root, Path::new("link/new/file")),
            Err(ConfineError::Outside)
        );
        assert_eq!(
            confine(&root, Path::new("link")),
            Err(ConfineError::Outside)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_is_unresolvable_not_trusted() -> TestResult {
        let (_temp, root, outside) = fixture()?;
        std::os::unix::fs::symlink(outside.join("absent"), root.join("dangling"))?;
        assert_eq!(
            confine(&root, Path::new("dangling")),
            Err(ConfineError::Unresolvable)
        );
        assert_eq!(
            confine(&root, Path::new("dangling/file")),
            Err(ConfineError::Unresolvable)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_the_root_is_followed_and_rechecked() -> TestResult {
        let (_temp, root, _outside) = fixture()?;
        std::fs::create_dir(root.join("real"))?;
        std::os::unix::fs::symlink(root.join("real"), root.join("alias"))?;
        let confined = confine(&root, Path::new("alias/file"))?;
        assert_eq!(
            confined.resolved(),
            std::fs::canonicalize(&root)?.join("real/file")
        );
        confined.recheck(&root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recheck_catches_a_link_swapped_in_after_resolution() -> TestResult {
        let (_temp, root, outside) = fixture()?;
        let confined = confine(&root, Path::new("staged/file"))?;
        std::os::unix::fs::symlink(&outside, root.join("staged"))?;
        assert_eq!(confined.recheck(&root), Err(ConfineError::Outside));
        std::fs::remove_file(root.join("staged"))?;
        std::fs::create_dir(root.join("elsewhere"))?;
        std::os::unix::fs::symlink(root.join("elsewhere"), root.join("staged"))?;
        assert_eq!(confined.recheck(&root), Err(ConfineError::Changed));
        Ok(())
    }
}
