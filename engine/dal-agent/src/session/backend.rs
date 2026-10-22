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
    JobsReply, MailMode, ModelRequest, ModelRoute, Name, Notice, Owner, Part, PrivateEffect,
    RawJson, RedirectPolicy, Reply, Request, SessionId, StateError, StateOp, StateRecord, TurnOp,
    TurnOpReply, Workspace,
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

/// Derives the owning extension identity from the host-minted caller.
///
/// The only input is the host-minted [`crate::ext::Caller`]; no layer
/// accepts a caller-supplied owner, so same-name extensions under different
/// origins stay disjoint by construction. Origin text renders through the
/// single shared renderer.
pub(crate) fn private_owner_of(caller: &crate::ext::Caller) -> Owner {
    Owner::Extension {
        name: caller.ext().as_str().into(),
        origin: crate::ext::services::origin_text_shared(caller.origin()).into(),
    }
}

/// Encodes the durable start policy of one child session: its tool
/// allowlist, the approval mode it inherits from its parent, and the
/// prompt customization its start carried. Missing optional fields keep
/// older records readable.
fn child_policy_body(
    tools: Option<&[Name]>,
    approval: ApprovalMode,
    role: Option<&str>,
    system: Option<&str>,
) -> Result<RawJson, ServiceError> {
    #[derive(serde::Serialize)]
    struct Policy<'a> {
        tools: Option<Vec<&'a str>>,
        approval: ApprovalMode,
        #[serde(skip_serializing_if = "Option::is_none")]
        role: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        system: Option<&'a str>,
    }
    let tools = tools.map(|names| names.iter().map(Name::as_str).collect());
    let body = sonic_rs::to_string(&Policy {
        tools,
        approval,
        role,
        system,
    })
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

/// Keeps the live child snapshot in step with the durable start policy
/// before the first prompt enters the child.
fn apply_child_snapshot(child: &crate::agent::Agent, start: &dal_core::AgentStart) {
    if let Some(names) = &start.tools {
        child.inner.shared.restrict_tools(names);
    }
    if let Some(system) = &start.system {
        child.inner.shared.set_child_prompt_policy(
            crate::session::shared::ChildPromptPolicy::System(system.clone()),
        );
    } else if let Some(role) = &start.role {
        child.inner.shared.set_child_prompt_policy(
            crate::session::shared::ChildPromptPolicy::Role(role.clone()),
        );
    }
}

/// Runs the prompted turn's interrupt inside the child's task group: after
/// `delay` the named turn is cancelled, and the timer dies with the session.
fn spawn_prompt_interrupt(
    tasks: &SessionTasks,
    handle: SessionHandle,
    turn: dal_core::TurnId,
    delay: std::time::Duration,
) {
    tasks.spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = handle
            .submit(
                Command::Cancel {
                    scope: dal_core::CancelScope::Turn(turn),
                },
                dal_core::ClientId::new("core"),
            )
            .await;
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
/// tail. Returns `None` when a component exists but cannot be resolved, or
/// when the tail would live under a non-directory ancestor.
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
                if !tail.is_empty() && !resolved.is_dir() {
                    return None;
                }
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

/// Resolves an absolute `raw` to a file under the session's own `jobs/`
/// directory, the only absolute location `fs.read` serves: the exec tool
/// prints its job log as `Full output: <jobs dir>/<job id>.log`, outside the
/// workspace, and a reader of that line must reach the file.
///
/// `Ok(None)` means nothing exists there. Links are resolved, so one that
/// leaves the directory is refused like any other outside path.
///
/// # Errors
///
/// Returns [`ConfineError::Outside`] when `raw` is not below `jobs_dir`, is
/// `jobs_dir` itself, or resolves outside it; [`ConfineError::ParentDir`]
/// when the part below `jobs_dir` climbs with `..`; and
/// [`ConfineError::Unresolvable`] when a component exists but cannot be
/// resolved.
pub(crate) fn resolve_job_log(
    jobs_dir: &Path,
    raw: &Path,
) -> Result<Option<PathBuf>, ConfineError> {
    let relative = raw
        .strip_prefix(jobs_dir)
        .map_err(|_| ConfineError::Outside)?;
    let mut names = 0_usize;
    for component in relative.components() {
        match component {
            Component::Normal(_) => names += 1,
            Component::CurDir => {}
            _ => return Err(ConfineError::ParentDir),
        }
    }
    if names == 0 {
        return Err(ConfineError::Outside);
    }
    let canonical_jobs = match std::fs::canonicalize(jobs_dir) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ConfineError::Unresolvable),
    };
    let resolved = match std::fs::canonicalize(raw) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ConfineError::Unresolvable),
    };
    if resolved != canonical_jobs && resolved.starts_with(&canonical_jobs) {
        Ok(Some(resolved))
    } else {
        Err(ConfineError::Outside)
    }
}

/// Reads one job log by absolute path on a blocking thread, then the bytes.
async fn read_job_log(jobs_dir: PathBuf, path: &str) -> Result<Option<Vec<u8>>, ServiceError> {
    let raw = PathBuf::from(path);
    let outcome = tokio::task::spawn_blocking(move || resolve_job_log(&jobs_dir, &raw))
        .await
        .map_err(|error| ServiceError::failed(Some(Service::FsRead), error.to_string()))?;
    let Some(resolved) = outcome.map_err(|error| refusal(Service::FsRead, path, error))? else {
        return Ok(None);
    };
    match tokio::fs::read(resolved).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(fs_failure(Service::FsRead, path, &error)),
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
        // The snapshot reaches every spawned child underneath the request
        // overrides, so host-borne git redirectors (`GIT_DIR`,
        // `GIT_CONFIG_*`, …) would silently move git outside the grant roots
        // with no argv evidence. Denying on ambient operator configuration at
        // each spawn gate would turn environments into per-call failures, so
        // the redirect set is scrubbed once here instead; children fall back
        // to normal cwd-anchored discovery. Request overrides are still
        // vetted per spawn by the grant gates.
        let env_snapshot = host
            .shared
            .env
            .vars
            .iter()
            .filter(|(key, _)| !is_git_redirect_var(key))
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

    /// Acquires the live session services without retaining them between calls.
    pub(crate) fn services(&self) -> Option<Arc<dyn crate::ext::Services>> {
        self.script_services()
            .map(|services| services as Arc<dyn crate::ext::Services>)
    }

    /// Publishes the session services once the session start builds them.
    pub(crate) fn set_services(&self, services: &Arc<SessionServices>) {
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

/// Reports whether an environment variable can redirect git outside the grant
/// roots without argv evidence.
///
/// This mirrors the request-override vetting in `dispatch::git_env_in_roots`:
/// path-bearing selectors, the `git config` file path, the ref namespace,
/// Git's alternate config files, its internal `-c` relay channel, and the
/// env-borne `-c` scheme. Anything else (identity, protocol policy, tracing,
/// …) passes through untouched.
fn is_git_redirect_var(key: &std::ffi::OsString) -> bool {
    let Some(key) = key.to_str() else {
        return false;
    };
    // Environment names resolve case-insensitively on Windows, so lowercase
    // spellings reach git under the canonical names there: match ASCII
    // case-insensitively everywhere.
    key.eq_ignore_ascii_case("GIT_DIR")
        || key.eq_ignore_ascii_case("GIT_WORK_TREE")
        || key.eq_ignore_ascii_case("GIT_NAMESPACE")
        || key.eq_ignore_ascii_case("GIT_COMMON_DIR")
        || key.eq_ignore_ascii_case("GIT_EXEC_PATH")
        || key.eq_ignore_ascii_case("GIT_OBJECT_DIRECTORY")
        || key.eq_ignore_ascii_case("GIT_INDEX_FILE")
        || key.eq_ignore_ascii_case("GIT_CONFIG")
        || key.eq_ignore_ascii_case("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        || key.eq_ignore_ascii_case("GIT_CONFIG_GLOBAL")
        || key.eq_ignore_ascii_case("GIT_CONFIG_SYSTEM")
        || key.eq_ignore_ascii_case("GIT_CONFIG_COUNT")
        || key.eq_ignore_ascii_case("GIT_CONFIG_PARAMETERS")
        || key
            .get(.."GIT_CONFIG_KEY_".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("GIT_CONFIG_KEY_"))
        || key
            .get(.."GIT_CONFIG_VALUE_".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("GIT_CONFIG_VALUE_"))
}

impl SessionBackend for Backend {
    fn fs_read(&self, path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        let root = self.canonical_root.clone();
        let jobs_dir = self.jobs_dir.clone();
        let path = path.to_owned();
        Box::pin(async move {
            if Path::new(&path).is_absolute() {
                return read_job_log(jobs_dir, &path).await;
            }
            let confined = confine_path(root.clone(), &path, Service::FsRead).await?;
            let confined = recheck_path(root, confined, &path, Service::FsRead).await?;
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
            let client = match req.redirects {
                RedirectPolicy::Follow => dal_provider::build_client(),
                RedirectPolicy::Refuse => dal_provider::build_client_no_redirect(),
            };
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
        let Some(services) = self.services() else {
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

    fn ext_records(&self) -> Arc<[ExtRecord]> {
        self.shared.ext_records()
    }
    /// Returns the terminal private-effect facts the calling extension owns,
    /// oldest first, from the published terminal index; never waits on the
    /// actor.
    ///
    /// The owner is derived here from the host-minted caller at the filter
    /// site. The predicate runs inside the shared lock over the shared
    /// payloads; only owner-matched payloads are mapped to rows outside it.
    fn private_outcomes(&self, who: &crate::ext::Caller) -> ServiceFuture<'_, Vec<PrivateEffect>> {
        let owner = private_owner_of(who);
        let rows = self
            .shared
            .private_terminal_select(|terminal| terminal.who == owner);
        Box::pin(async move {
            Ok(rows
                .iter()
                .map(|terminal| PrivateEffect::from(terminal.as_ref()))
                .collect())
        })
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

    fn model_ready(&self, model: ModelRoute) -> ServiceFuture<'_, Option<ModelRoute>> {
        let deps = super::turn::RequestDeps {
            session: self.session,
            host: Arc::clone(&self.host),
            script: None,
            // Pinned: service-only readiness has no turn and no invocation
            // owner, so it stays the one-shot fallback by construction.
            continuation: None,
        };
        let cancel = self.cancel.clone();
        Box::pin(async move { super::turn::model_ready(&deps, model, &cancel).await })
    }

    fn infer(
        &self,
        caller: &crate::ext::Caller,
        req: ModelRequest,
    ) -> ServiceFuture<'_, Inference> {
        // `caller` is a short borrow that must not cross awaits: clone the
        // small, fully owned Caller so the future below owns it instead —
        // the same clone-first idiom SessionServices uses for `who`.
        let caller = caller.clone();
        Box::pin(async move {
            let deps = self.infer_deps(&caller);
            let stream = super::turn::infer_stream(&deps, req, &self.cancel).await;
            crate::ext::synthetic::collect(stream)
                .await
                .map_err(|failure| crate::error::ServiceError::failed(None, failure.to_string()))
        })
    }

    fn infer_with_script(
        &self,
        caller: &crate::ext::Caller,
        req: ModelRequest,
        script: Option<Arc<crate::session::script::SessionScriptHost>>,
        cancel: CancellationToken,
    ) -> ServiceFuture<'_, Inference> {
        // Same owned-clone rationale as `infer`: no short borrow crosses the
        // pin boundary; the literal below borrows future-owned data only.
        let caller = caller.clone();
        Box::pin(async move {
            let deps = super::turn::RequestDeps {
                session: self.session,
                host: Arc::clone(&self.host),
                script,
                continuation: caller.continuation(),
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

    fn infer_stream(
        &self,
        caller: &crate::ext::Caller,
        req: ModelRequest,
    ) -> ServiceFuture<'_, EventStream> {
        // Same owned-clone rationale as `infer`: the temporary RequestDeps
        // built inside the future is future-owned, and `caller` is too.
        let caller = caller.clone();
        Box::pin(async move {
            Ok(super::turn::infer_stream(&self.infer_deps(&caller), req, &self.cancel).await)
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

/// One child session owned by the parent invocation that is starting or
/// awaiting it.
///
/// Armed after a start opens the child, or while an invocation waits on
/// its child, the guard owns the caller-side lifetime of that child. It
/// disarms on every completed outcome: a started child, a report, a
/// deadline, or a refused wait. It fires only when the owning future is
/// dropped mid-flight: the caller's turn was cancelled or the invocation
/// aborted. Firing removes the child from the live table at once, so the
/// parent-scoped list and child admission observe the reclaim
/// synchronously, then parks the subtree teardown tail, the child's own
/// started-but-unawaited descendants swept first and every child before
/// its parent, on the owning session's task owner, which its own close
/// and the host shutdown always drain. An entry already reclaimed
/// elsewhere, by an explicit `agents.cancel`, a failed start's close, or a
/// close sweep, leaves the guard nothing to do.
struct OwnedChild {
    tasks: SessionTasks,
    host: Arc<HostState>,
    id: SessionId,
    /// The runtime captured while the await was armed: the drop may run
    /// outside a polled context, so it never asks the thread for one.
    runtime: tokio::runtime::Handle,
    armed: bool,
}

impl OwnedChild {
    fn arm(tasks: SessionTasks, host: Arc<HostState>, id: SessionId) -> Self {
        Self {
            tasks,
            host,
            id,
            runtime: tokio::runtime::Handle::current(),
            armed: true,
        }
    }

    /// Ends this invocation's ownership: the start returned the child, or
    /// the wait completed, timed out, or was refused, so the child outlives
    /// the invocation on purpose.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let entry = self
            .host
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
        let Some(entry) = entry else {
            return;
        };
        let id = self.id;
        let host = Arc::clone(&self.host);
        self.tasks.spawn_cleanup(&self.runtime, async move {
            crate::host::Host::teardown_subtree(&host, entry, id).await;
        });
    }
}

impl Backend {
    /// Provider-wire name of the trusted child completion tool. It follows
    /// the `<plugin>__<local>` wire convention, and the `orchestration`
    /// extension name is already claimed by the Bundled battery, so a User
    /// plugin cannot mint the same wire name without an extension-name
    /// conflict; plain `report` no longer matches.
    const CHILD_REPORT_TOOL_NAME: &str = "orchestration__report";

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
            } => self.agent_send(to, text, mode, reply_to).await,
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
    ///
    /// Worktree-isolated children are the one exception: the orchestration
    /// battery creates each task's worktree under the host data root at
    /// `<data root>/worktrees/<session>/<task>` and starts the child
    /// inside it, outside the caller's workspace by design. A child
    /// workspace under this session's own worktree root admits; anything
    /// else under the data root — another session's trees, the isolation
    /// receipts, the session stores — still refuses with
    /// [`AgentRefusal::WorkspaceOutsideRoot`].
    fn child_workspace(&self, requested: Option<&Workspace>) -> Result<Workspace, AgentRefusal> {
        let workspace = requested.unwrap_or(&self.workspace);
        let child_root = std::fs::canonicalize(workspace.as_path())
            .map_err(|_| AgentRefusal::WorkspaceUnresolved)?;
        let in_workspace = child_root.starts_with(&self.canonical_root);
        let in_worktrees = self
            .session_worktree_root()
            .is_some_and(|root| child_root.starts_with(&root));
        if !in_workspace && !in_worktrees {
            return Err(AgentRefusal::WorkspaceOutsideRoot);
        }
        Workspace::new(child_root).map_err(|_| AgentRefusal::WorkspaceUnresolved)
    }

    /// The canonical `<data root>/worktrees/<session>` root this session's
    /// isolated task worktrees live under, when it resolves. `None` keeps
    /// the workspace-only boundary: a session with no worktree area admits
    /// no data-root child. The child workspace itself must already exist
    /// to reach this check, so its worktree root resolves too — anything
    /// unresolvable fails closed here exactly like the workspace root.
    fn session_worktree_root(&self) -> Option<PathBuf> {
        std::fs::canonicalize(
            self.host
                .shared
                .data_root
                .join("worktrees")
                .join(self.session.to_string()),
        )
        .ok()
    }

    async fn agent_start(&self, start: dal_core::AgentStart) -> Result<AgentsReply, ServiceError> {
        if let Err(error) = start.validate() {
            return Err(child_start_error(error));
        }
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
                    name: start.name.clone(),
                },
                dal_core::ClientId::new("core"),
            )
            .await
            .map_err(child_start_error)?;
        let child_id = child.inner.session;
        // Until the start returns, this invocation owns the child: a start
        // dropped mid-flight parks its teardown on this session's tasks.
        let mut owned = OwnedChild::arm(self.tasks.clone(), Arc::clone(&self.host), child_id);
        // The child is restricted before its first turn: a start that sets
        // `tools` runs with exactly those tools, on every turn and across
        // reloads, and an absent list leaves the child unrestricted.
        // The restriction and the inherited approval mode are journaled
        // through the child's actor first: a start whose policy cannot be
        // recorded leaves no half-restricted child behind.
        let Ok(body) = child_policy_body(
            start.tools.as_deref(),
            self.shared.approval(),
            start.role.as_deref(),
            start.system.as_deref(),
        ) else {
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
        apply_child_snapshot(&child, &start);
        // The child starts under the approval mode its parent runs under now,
        // not the configured default. When the mode cannot be set the child
        // does not start.
        let approval = self.shared.approval();
        if let Err(error) = copy_child_approval(&child, approval).await {
            let _ = self.host().close(child_id).await;
            return Err(child_start_error(error));
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
                content: vec![Part::Text { text: start.prompt }],
            })
            .await
        {
            let _ = host.close(child_id).await;
            return Err(child_start_error(error));
        }
        owned.disarm();
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
        let Ok(Reply::Accepted { turn, .. }) = reply else {
            child_shared.set_next_turn_step_cap(None);
            self.release_prompt_slot(id);
            return AgentsReply::Cancelled { id };
        };
        if let Some(delay) = interrupt {
            spawn_prompt_interrupt(&child_tasks, handle, turn, delay);
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

    /// Parks the child await between idle polls until the next poll is due.
    ///
    /// Waits for cancellation, the await deadline, or a short poll interval.
    /// Returns the terminal reply when the wait itself settles the await;
    /// returns `None` when the caller should poll the child again.
    async fn pause_between_child_polls(
        &self,
        id: SessionId,
        deadline: Option<tokio::time::Instant>,
    ) -> Option<AgentsReply> {
        if let Some(deadline) = deadline {
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => Some(AgentsReply::Cancelled { id }),
                () = tokio::time::sleep_until(deadline) => Some(AgentsReply::Pending { id }),
                () = tokio::time::sleep(std::time::Duration::from_millis(250)) => None,
            }
        } else {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            None
        }
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
        // The guard owns this invocation's link to the child from here on:
        // every completed outcome below disarms it, and only a future
        // dropped mid-await fires it, reclaiming the outstanding child.
        let mut owned = OwnedChild::arm(self.tasks.clone(), Arc::clone(&self.host), id);
        let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
        loop {
            if self.cancel.is_cancelled() {
                owned.disarm();
                return AgentsReply::Cancelled { id };
            }
            let (reply, response) = oneshot::channel();
            let idle_query = async {
                agent
                    .inner
                    .handle
                    .turn(crate::session::TurnRequest {
                        op: TurnOp::IsIdle,
                        reply,
                    })
                    .await
                    .ok()?;
                response.await.ok()
            };
            let idle = tokio::select! {
                biased;
                () = self.cancel.cancelled() => {
                    owned.disarm();
                    return AgentsReply::Cancelled { id };
                }
                () = async {
                    match deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => {
                    owned.disarm();
                    return AgentsReply::Pending { id };
                }
                response = idle_query => if let Some(TurnOpReply::Idle(idle)) = response {
                    idle
                } else {
                    owned.disarm();
                    return AgentsReply::Cancelled { id };
                },
            };
            let Ok(view) = agent.view(dal_core::PageReq::default()) else {
                owned.disarm();
                return AgentsReply::Cancelled { id };
            };
            if idle && matches!(view.turn, dal_core::TurnState::Idle) {
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
                owned.disarm();
                return report;
            }
            if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                owned.disarm();
                return AgentsReply::Pending { id };
            }
            if let Some(reply) = self.pause_between_child_polls(id, deadline).await {
                owned.disarm();
                return reply;
            }
        }
    }

    /// Builds the completion report from the child's last assistant entry.
    /// `stop` is the child's durable terminal stop, read off its projection;
    /// `view.turn` alone only knows the turn ended, not why. An explicit
    /// `orchestration__report` tool call on any assistant entry wins over the
    /// last text.
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
                    // The report reads visible text only; the opaque
                    // provider replay payload rides along untouched.
                    if let dal_core::Block::Text { text: chunk, .. } = block {
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
                (name.as_ref() == Self::CHILD_REPORT_TOOL_NAME)
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
    ) -> Result<AgentsReply, ServiceError> {
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
                return Ok(AgentsReply::Delivered(dal_core::ext::Receipt::Gone));
            }
            sessions
                .get(&to)
                .filter(|entry| !entry.reported.load(std::sync::atomic::Ordering::SeqCst))
                .map(|entry| entry.handle.clone())
        };
        let Some(handle) = handle else {
            return Ok(AgentsReply::Delivered(dal_core::ext::Receipt::Gone));
        };
        let (tx, rx) = oneshot::channel();
        let outcome = handle
            .mail(crate::session::MailRequest::Send { mail, reply: tx })
            .await;
        match (outcome, rx.await) {
            (Ok(()), Ok(result)) => result.map(|receipt| {
                AgentsReply::Delivered(receipt.unwrap_or(dal_core::ext::Receipt::Gone))
            }),
            _ => Ok(AgentsReply::Delivered(dal_core::ext::Receipt::Gone)),
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

    /// Builds the request inputs for caller-attributed inference.
    ///
    /// The continuation is whatever the host-minted caller carries: `Some`
    /// when an invocation root minted an owner, `None` for service-only
    /// inference, which stays the one-shot fallback by construction — never
    /// a refusal.
    fn infer_deps(&self, caller: &crate::ext::Caller) -> super::turn::RequestDeps {
        super::turn::RequestDeps {
            session: self.session,
            host: Arc::clone(&self.host),
            script: None,
            continuation: caller.continuation(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;
    use crate::host::{Env, Host, Product, SessionRef};
    use crate::session::control::ControlCell;
    use crate::session::{ActorRequest, ResolutionInbox};
    use dal_core::{ApprovalMode, CallId, ClientId, Config, ConfigProduct, Workspace};

    #[tokio::test]
    async fn prompt_interrupt_targets_ended_turn_without_cancelling_successor() {
        let prompted = dal_core::TurnId::new(NonZeroU64::MIN);
        let successor = dal_core::TurnId::new(NonZeroU64::new(2).expect("nonzero turn"));
        // The constructor under test shares one broker between the inbox
        // and the cell, as `spawn` wires them: claims route through the
        // inbox under its lock.
        let inbox = Arc::new(ResolutionInbox::new(Arc::new(Broker::new())));
        let control = Arc::new(std::sync::Mutex::new(ControlCell::new(Arc::clone(&inbox))));
        let successor_token = {
            let mut control = control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            control.begin_turn(prompted);
            control.end_turn(prompted);
            control.begin_turn(successor)
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let (opening_tx, _opening_rx) = tokio::sync::mpsc::channel(1);
        let handle = SessionHandle::new(
            SessionId::new_v7(),
            tx,
            opening_tx,
            Arc::clone(&control),
            inbox,
        );
        let tasks = SessionTasks::new();
        spawn_prompt_interrupt(&tasks, handle, prompted, std::time::Duration::ZERO);

        let request = rx.recv().await.expect("timer submits a cancellation");
        let ActorRequest::Submit { command, reply, .. } = request else {
            panic!("timer submitted an unrelated actor request");
        };
        assert_eq!(
            command,
            Command::Cancel {
                scope: dal_core::CancelScope::Turn(prompted),
            }
        );
        reply
            .send(Ok(Reply::Done(dal_core::Output::Nothing)))
            .expect("timer is waiting for the command reply");
        assert!(!successor_token.is_cancelled());
        tasks.stop().await;
    }

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
            name: Some("member".into()),
            prompt: "work".into(),
            model: model.map(Into::into),
            role: None,
            system: None,
            tools: None,
            workspace,
        }
    }

    #[tokio::test]
    async fn a_start_dropped_after_opening_its_child_reclaims_the_child() {
        let (host, root, _child, _temp) = host_with_child().await;
        let backend = backend_of(&host, root);
        let live = || {
            host.state
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len()
        };
        let before = live();
        let mut start = Box::pin(backend.agents_op(AgentsOp::Start(start_in(None, None))));
        loop {
            let polled =
                std::future::poll_fn(|cx| std::task::Poll::Ready(start.as_mut().poll(cx))).await;
            assert!(
                polled.is_pending(),
                "the start finished before it was dropped"
            );
            if live() > before {
                break;
            }
            tokio::task::yield_now().await;
        }
        drop(start);
        assert_eq!(live(), before, "the dropped start left its child live");
        host.close(root).await.expect("close session");
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
    async fn a_child_in_the_session_worktree_tree_starts() {
        // Worktree-isolated children live under the host data root at
        // `<data>/worktrees/<session>/<task>`, outside the caller's
        // workspace by design: the orchestration battery creates the tree
        // and starts the child inside it. The session's own worktree tree
        // admits; the general outside-root refusal below is unchanged.
        let (host, root, _child, temp) = host_with_child().await;
        let backend = backend_of(&host, root);
        let tree = temp
            .path()
            .join("data")
            .join("worktrees")
            .join(root.to_string())
            .join("task");
        std::fs::create_dir_all(&tree).expect("worktree tree");
        let workspace = Workspace::new(tree).expect("worktree workspace");
        let reply = backend
            .agents_op(AgentsOp::Start(start_in(Some(workspace), None)))
            .await
            .expect("a start answers");
        let AgentsReply::Started { id } = reply else {
            panic!("a worktree child must start, got {reply:?}");
        };
        host.close(id).await.expect("close child");
        host.close(root).await.expect("close session");
    }

    #[tokio::test]
    async fn data_root_paths_outside_the_session_worktree_tree_still_refuse() {
        let (host, root, _child, temp) = host_with_child().await;
        let backend = backend_of(&host, root);
        // Another session's worktree tree: the right shape, the wrong owner.
        let foreign = temp
            .path()
            .join("data")
            .join("worktrees")
            .join("01900000-0000-7000-8000-000000000000")
            .join("task");
        assert_ne!(root.to_string(), "01900000-0000-7000-8000-000000000000");
        // The isolation receipts live under the data root but are artifacts,
        // never child workspaces.
        let receipts = temp
            .path()
            .join("data")
            .join("isolation")
            .join(root.to_string())
            .join("task");
        std::fs::create_dir_all(&foreign).expect("foreign tree");
        std::fs::create_dir_all(&receipts).expect("receipts");
        for workspace in [foreign, receipts] {
            let workspace = Workspace::new(workspace).expect("workspace value");
            let reply = backend
                .agents_op(AgentsOp::Start(start_in(Some(workspace), None)))
                .await
                .expect("a refusal is a reply");
            assert_eq!(
                reply,
                AgentsReply::Refused {
                    reason: AgentRefusal::WorkspaceOutsideRoot
                }
            );
        }
        host.close(root).await.expect("close session");
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

    #[test]
    fn snapshot_scrub_covers_the_git_redirect_set() {
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_NAMESPACE",
            "GIT_COMMON_DIR",
            "GIT_EXEC_PATH",
            "GIT_OBJECT_DIRECTORY",
            "GIT_INDEX_FILE",
            "GIT_CONFIG",
            "git_config",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_12",
            // Windows resolves environment names case-insensitively.
            "git_dir",
            "Git_Namespace",
            "git_config_count",
            "git_config_key_0",
        ] {
            assert!(
                is_git_redirect_var(&std::ffi::OsString::from(name)),
                "{name} must be scrubbed from the snapshot"
            );
        }
        for name in [
            "PATH",
            "HOME",
            "GIT_CONFIG_KEY",
            "GIT_AUTHOR_NAME",
            "GIT_PAGER",
        ] {
            assert!(
                !is_git_redirect_var(&std::ffi::OsString::from(name)),
                "{name} must survive the snapshot scrub"
            );
        }
    }

    /// Private-effect actor boundary proofs: real actor plus temp-Store
    /// journals. Every write below travels `private_loop_born` /
    /// `private_start` / `private_end` through the live session actor and
    /// asserts against the file-backed journal; no fake journal or backend
    /// stands in. Owners are minted through `private_owner_of`, the same
    /// renderer the read filter uses, and every `Owner` value stays
    /// test-internal.
    fn private_test_caller(name: &str, origin: dal_core::Origin) -> crate::ext::Caller {
        crate::ext::Caller::new(
            name.parse::<dal_core::Name>()
                .expect("valid extension name"),
            origin,
            dal_core::ServiceSet::from_names(["ask"]).expect("inject parses"),
            std::num::NonZeroU32::MIN,
            crate::ext::CallerKind::Handler,
            None,
        )
        .with_test_digest()
    }

    fn private_journal_bytes(
        temp: &tempfile::TempDir,
        workspace: &Workspace,
        id: SessionId,
    ) -> Vec<u8> {
        let data_root = temp.path().join("data");
        let path = dal_store::Store::new(data_root, workspace.clone(), dal_core::Product::Dal)
            .session_file(id);
        std::fs::read(&path).expect("private journal is file-backed")
    }

    fn private_decode_records(bytes: &[u8]) -> Vec<dal_core::Record> {
        bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| dal_core::decode(line).expect("journal line decodes").record)
            .collect()
    }

    fn count_private_starts(
        records: &[dal_core::Record],
        want_loop: &dal_core::PrivateLoopId,
        want_token: &dal_core::PrivateEffectId,
    ) -> usize {
        records
            .iter()
            .filter(|record| {
                matches!(
                    record,
                    dal_core::Record::PrivateStart {
                        r#loop: found_loop,
                        token: found_token,
                        ..
                    } if found_loop == want_loop && found_token == want_token
                )
            })
            .count()
    }

    fn count_private_ends(
        records: &[dal_core::Record],
        want_loop: &dal_core::PrivateLoopId,
        want_token: &dal_core::PrivateEffectId,
    ) -> usize {
        records
            .iter()
            .filter(|record| {
                matches!(
                    record,
                    dal_core::Record::PrivateEnd {
                        r#loop: found_loop,
                        token: found_token,
                        ..
                    } if found_loop == want_loop && found_token == want_token
                )
            })
            .count()
    }

    fn private_settled_outcome() -> dal_core::PrivateOutcome {
        dal_core::PrivateOutcome::Settled {
            is_error: false,
            parts: vec![dal_core::JournalPart::Text { text: "ok".into() }],
        }
    }

    struct PrivateRig {
        host: Host,
        root: SessionId,
        child: SessionId,
        temp: tempfile::TempDir,
        workspace: Workspace,
        caller: crate::ext::Caller,
        owner: Owner,
        backend: Arc<Backend>,
        handle: SessionHandle,
        r#loop: dal_core::PrivateLoopId,
        token: dal_core::PrivateEffectId,
    }

    async fn private_rig_with_loop() -> PrivateRig {
        let (host, root, child, temp) = host_with_child().await;
        let workspace = Workspace::new(temp.path().join("workspace")).expect("workspace value");
        let caller = private_test_caller("focus", dal_core::Origin::User);
        let owner = private_owner_of(&caller);
        let backend = backend_of(&host, root);
        let handle = backend.handle().clone();
        let r#loop = dal_core::PrivateLoopId::new_v7();
        let token = dal_core::PrivateEffectId::new_v7();
        PrivateRig {
            host,
            root,
            child,
            temp,
            workspace,
            caller,
            owner,
            backend,
            handle,
            r#loop,
            token,
        }
    }

    async fn private_birth_loop(rig: &PrivateRig) {
        rig.handle
            .private_loop_born(rig.r#loop, rig.owner.clone(), None)
            .await
            .expect("loop birth receipts");
    }

    async fn private_start_effect(
        rig: &PrivateRig,
        round: u32,
        call: &str,
        tool: &str,
        args: &str,
    ) {
        rig.handle
            .private_start(
                rig.r#loop,
                rig.token,
                rig.owner.clone(),
                None,
                round,
                dal_core::CallId::new(call),
                tool.into(),
                dal_core::RawJson::parse(args).expect("args parse"),
            )
            .await
            .expect("start receipts");
    }

    async fn rejected_assert_duplicate_birth(rig: &PrivateRig, baseline: &[u8]) {
        // A duplicate birth is rejected and the journal stays byte-identical.
        assert!(
            rig.handle
                .private_loop_born(rig.r#loop, rig.owner.clone(), None)
                .await
                .is_err(),
            "a twice-born loop must fail"
        );
        assert_eq!(
            private_journal_bytes(&rig.temp, &rig.workspace, rig.root),
            baseline,
            "a rejected birth appends nothing"
        );
    }

    async fn rejected_assert_foreign_start(rig: &PrivateRig, baseline: &[u8]) {
        // A start under a foreign owner is rejected with no new bytes.
        let stranger = private_test_caller("stranger", dal_core::Origin::User);
        assert!(
            rig.handle
                .private_start(
                    rig.r#loop,
                    rig.token,
                    private_owner_of(&stranger),
                    None,
                    0,
                    dal_core::CallId::new("rejected-call"),
                    "proof-tool".into(),
                    dal_core::RawJson::parse("null").expect("args parse"),
                )
                .await
                .is_err(),
            "an owner-mismatched start must fail"
        );
        assert_eq!(
            private_journal_bytes(&rig.temp, &rig.workspace, rig.root),
            baseline,
            "a mismatched start appends nothing"
        );
    }

    async fn rejected_drive_valid_start(rig: &PrivateRig) -> Vec<u8> {
        // The one valid start receipts; its duplicate fails without a record.
        private_start_effect(rig, 0, "proof-call", "proof-tool", "null").await;
        let after_start = private_journal_bytes(&rig.temp, &rig.workspace, rig.root);
        assert_eq!(
            count_private_starts(
                &private_decode_records(&after_start),
                &rig.r#loop,
                &rig.token
            ),
            1,
            "exactly one start record lands"
        );
        assert!(
            rig.handle
                .private_start(
                    rig.r#loop,
                    rig.token,
                    rig.owner.clone(),
                    None,
                    0,
                    dal_core::CallId::new("proof-call"),
                    "proof-tool".into(),
                    dal_core::RawJson::parse("null").expect("args parse"),
                )
                .await
                .is_err(),
            "a twice-started effect must fail"
        );
        assert_eq!(
            private_journal_bytes(&rig.temp, &rig.workspace, rig.root),
            after_start,
            "a duplicate start appends nothing"
        );
        after_start
    }

    async fn rejected_drive_valid_end(rig: &PrivateRig) -> Vec<u8> {
        // One end settles the row; a second end for the token fails and the
        // journal still holds exactly one end record for it.
        rig.handle
            .private_end(rig.r#loop, rig.token, private_settled_outcome())
            .await
            .expect("valid end receipts");
        let after_end = private_journal_bytes(&rig.temp, &rig.workspace, rig.root);
        assert_eq!(
            count_private_ends(&private_decode_records(&after_end), &rig.r#loop, &rig.token),
            1,
            "exactly one end record lands"
        );
        assert!(
            rig.handle
                .private_end(rig.r#loop, rig.token, private_settled_outcome())
                .await
                .is_err(),
            "a twice-ended effect must fail"
        );
        assert_eq!(
            private_journal_bytes(&rig.temp, &rig.workspace, rig.root),
            after_end,
            "a duplicate end appends nothing"
        );
        after_end
    }

    async fn receipt_assert_start_durable_and_settle(rig: &PrivateRig) {
        // The receipt above is durable: the start record is on the file, and
        // only then does the row settle and publish its terminal.
        let records =
            private_decode_records(&private_journal_bytes(&rig.temp, &rig.workspace, rig.root));
        assert_eq!(
            count_private_starts(&records, &rig.r#loop, &rig.token),
            1,
            "the start receipt is on disk"
        );
        rig.handle
            .private_end(rig.r#loop, rig.token, private_settled_outcome())
            .await
            .expect("end receipts");
        let settled_rows = rig
            .backend
            .private_outcomes(&rig.caller)
            .await
            .expect("owner reads its terminal");
        assert_eq!(settled_rows.len(), 1, "the settled terminal publishes");
        assert_eq!(settled_rows[0].token, rig.token);
    }

    async fn receipt_assert_failed_start_rolls_back(rig: &PrivateRig) {
        // A failed append folds and publishes nothing: the armed write fault
        // rejects the start, the owner still reads only the settled row, and
        // the journal file is byte-identical (the partial batch rolls back).
        let faulted = dal_core::PrivateEffectId::new_v7();
        let before_fault = private_journal_bytes(&rig.temp, &rig.workspace, rig.root);
        dal_store::Store::new(
            rig.temp.path().join("data"),
            rig.workspace.clone(),
            dal_core::Product::Dal,
        )
        .arm_append_write_fault(rig.root, 0);
        assert!(
            matches!(
                rig.handle
                    .private_start(
                        rig.r#loop,
                        faulted,
                        rig.owner.clone(),
                        None,
                        1,
                        dal_core::CallId::new("faulted-call"),
                        "proof-tool".into(),
                        dal_core::RawJson::parse("null").expect("args parse"),
                    )
                    .await,
                Err(ServiceError::Failed { .. })
            ),
            "a failed append must reject the start"
        );
        let rows_after_fault = rig
            .backend
            .private_outcomes(&rig.caller)
            .await
            .expect("owner reads after the failed append");
        assert_eq!(
            rows_after_fault.len(),
            1,
            "the failed start publishes nothing"
        );
        assert_eq!(
            private_journal_bytes(&rig.temp, &rig.workspace, rig.root),
            before_fault,
            "the rolled-back batch leaves no bytes"
        );
    }

    async fn receipt_assert_closed_lane_rejects(rig: &PrivateRig) {
        // A closed lane rejects with typed errors.
        rig.host.close(rig.child).await.expect("close child");
        rig.host.close(rig.root).await.expect("close session");
        assert!(
            matches!(
                rig.handle
                    .private_start(
                        rig.r#loop,
                        dal_core::PrivateEffectId::new_v7(),
                        rig.owner.clone(),
                        None,
                        1,
                        dal_core::CallId::new("closed-call"),
                        "proof-tool".into(),
                        dal_core::RawJson::parse("null").expect("args parse"),
                    )
                    .await,
                Err(ServiceError::Failed { .. })
            ),
            "a start on a closed session must fail"
        );
        assert!(
            matches!(
                rig.handle
                    .private_end(rig.r#loop, rig.token, private_settled_outcome())
                    .await,
                Err(ServiceError::Failed { .. })
            ),
            "an end on a closed session must fail"
        );
        assert!(
            matches!(
                rig.handle
                    .private_loop_born(dal_core::PrivateLoopId::new_v7(), rig.owner.clone(), None)
                    .await,
                Err(AgentError::SessionClosed { .. })
            ),
            "a birth on a closed session must fail"
        );
    }

    async fn unfinished_assert_failed_end_publishes_nothing(rig: &PrivateRig) {
        // The row stays Started with no end landed; then the armed write
        // fault rejects the end, which must fold and publish nothing.
        dal_store::Store::new(
            rig.temp.path().join("data"),
            rig.workspace.clone(),
            dal_core::Product::Dal,
        )
        .arm_append_write_fault(rig.root, 0);
        assert!(
            matches!(
                rig.handle
                    .private_end(rig.r#loop, rig.token, private_settled_outcome())
                    .await,
                Err(ServiceError::Failed { .. })
            ),
            "a failed end append must reject"
        );
        let rows_before_close = rig
            .backend
            .private_outcomes(&rig.caller)
            .await
            .expect("owner reads before close");
        assert!(
            rows_before_close.is_empty(),
            "the failed end publishes no terminal"
        );
        let records_before_close =
            private_decode_records(&private_journal_bytes(&rig.temp, &rig.workspace, rig.root));
        assert_eq!(
            count_private_ends(&records_before_close, &rig.r#loop, &rig.token),
            0,
            "the failed end appends no record"
        );
    }

    async fn unfinished_assert_reopen_repairs_once(rig: &PrivateRig) {
        // Reopen repairs the unresolved start to exactly one Indeterminate
        // end on disk; the owner reads that terminal, never a Settled row.
        let reopened = rig
            .host
            .open(
                SessionRef::Resume {
                    key: rig.root.to_string().into(),
                    workspace: rig.workspace.clone(),
                },
                ClientId::new("backend-test"),
            )
            .await
            .expect("reopen session");
        assert_eq!(
            reopened.session(),
            rig.root,
            "resume reopens the same session"
        );
        let rows = backend_of(&rig.host, rig.root)
            .private_outcomes(&rig.caller)
            .await
            .expect("owner reads its terminals");
        assert_eq!(rows.len(), 1, "one repair terminal publishes");
        assert_eq!(rows[0].r#loop, rig.r#loop);
        assert_eq!(rows[0].token, rig.token);
        assert_eq!(rows[0].round, 3);
        assert!(
            matches!(rows[0].outcome, dal_core::PrivateOutcome::Indeterminate),
            "an unfinished effect reads Indeterminate, never Settled"
        );
        let repaired = private_journal_bytes(&rig.temp, &rig.workspace, rig.root);
        assert_eq!(
            count_private_ends(&private_decode_records(&repaired), &rig.r#loop, &rig.token),
            1,
            "the reopen persists exactly one repair end"
        );

        rig.host
            .close(rig.root)
            .await
            .expect("close repaired session");
    }

    async fn unfinished_assert_second_reopen_repairs_nothing(rig: &PrivateRig) {
        let reopened_again = rig
            .host
            .open(
                SessionRef::Resume {
                    key: rig.root.to_string().into(),
                    workspace: rig.workspace.clone(),
                },
                ClientId::new("backend-test"),
            )
            .await
            .expect("second reopen");
        assert_eq!(reopened_again.session(), rig.root);
        let reopened_records =
            private_decode_records(&private_journal_bytes(&rig.temp, &rig.workspace, rig.root));
        assert_eq!(
            count_private_ends(&reopened_records, &rig.r#loop, &rig.token),
            1,
            "the second reopen persists no extra repair"
        );
        let rows_again = backend_of(&rig.host, rig.root)
            .private_outcomes(&rig.caller)
            .await
            .expect("owner reads after second reopen");
        assert_eq!(rows_again.len(), 1, "still exactly one terminal");
        assert_eq!(rows_again[0].r#loop, rig.r#loop);
        assert_eq!(rows_again[0].token, rig.token);
        assert_eq!(rows_again[0].round, 3);
        assert!(
            matches!(
                rows_again[0].outcome,
                dal_core::PrivateOutcome::Indeterminate
            ),
            "the terminal stays Indeterminate"
        );

        rig.host.close(rig.root).await.expect("close session");
    }

    #[tokio::test]
    async fn private_rejected_transitions_persist_nothing() {
        let rig = private_rig_with_loop().await;
        private_birth_loop(&rig).await;
        let baseline = private_journal_bytes(&rig.temp, &rig.workspace, rig.root);
        rejected_assert_duplicate_birth(&rig, &baseline).await;
        rejected_assert_foreign_start(&rig, &baseline).await;
        let _after_start = rejected_drive_valid_start(&rig).await;
        let _after_end = rejected_drive_valid_end(&rig).await;

        // The rejections above left the fold untouched: an end for a token
        // that never started still fails instead of settling a phantom row.
        assert!(
            rig.handle
                .private_end(
                    rig.r#loop,
                    dal_core::PrivateEffectId::new_v7(),
                    private_settled_outcome()
                )
                .await
                .is_err(),
            "an end without a start must fail"
        );

        rig.host.close(rig.child).await.expect("close child");
        rig.host.close(rig.root).await.expect("close session");
    }

    #[tokio::test]
    async fn private_receipt_persisted_and_closed_operations_reject() {
        let rig = private_rig_with_loop().await;
        private_birth_loop(&rig).await;
        private_start_effect(&rig, 0, "proof-call", "proof-tool", "null").await;
        receipt_assert_start_durable_and_settle(&rig).await;
        receipt_assert_failed_start_rolls_back(&rig).await;
        receipt_assert_closed_lane_rejects(&rig).await;
    }

    #[tokio::test]
    async fn private_unfinished_start_repairs_once_across_reopen() {
        let rig = private_rig_with_loop().await;
        private_birth_loop(&rig).await;
        private_start_effect(&rig, 3, "wire-call-9", "tool-a", r#"{"n":9}"#).await;
        unfinished_assert_failed_end_publishes_nothing(&rig).await;
        rig.host.close(rig.child).await.expect("close child");
        rig.host.close(rig.root).await.expect("close session");
        unfinished_assert_reopen_repairs_once(&rig).await;
        unfinished_assert_second_reopen_repairs_nothing(&rig).await;
    }

    #[tokio::test]
    async fn private_end_only_advance_is_visible_to_the_owner_read() {
        let (host, root, child, _temp) = host_with_child().await;
        let caller = private_test_caller("focus", dal_core::Origin::User);
        let owner = private_owner_of(&caller);
        let backend = backend_of(&host, root);
        let handle = backend.handle().clone();

        // Two loops settle with no leaf or ext movement at all: only private
        // records land, so a publication parked behind the `sync_ext` memo
        // early-return would stay stuck at one row.
        for (round, tool) in [(0_u32, "tool-a"), (1_u32, "tool-b")] {
            let r#loop = dal_core::PrivateLoopId::new_v7();
            let token = dal_core::PrivateEffectId::new_v7();
            handle
                .private_loop_born(r#loop, owner.clone(), None)
                .await
                .expect("loop birth receipts");
            handle
                .private_start(
                    r#loop,
                    token,
                    owner.clone(),
                    None,
                    round,
                    dal_core::CallId::new("proof-call"),
                    tool.into(),
                    dal_core::RawJson::parse("null").expect("args parse"),
                )
                .await
                .expect("start receipts");
            handle
                .private_end(r#loop, token, private_settled_outcome())
                .await
                .expect("end receipts");
            let rows = backend
                .private_outcomes(&caller)
                .await
                .expect("owner reads its terminals");
            assert_eq!(
                rows.len(),
                usize::try_from(round).expect("round fits") + 1,
                "each End-only advance publishes"
            );
        }
        let rows = backend
            .private_outcomes(&caller)
            .await
            .expect("final owner read");
        assert_eq!(rows.len(), 2);
        assert_eq!(&*rows[0].name, "tool-a");
        assert_eq!(&*rows[1].name, "tool-b");

        host.close(child).await.expect("close child");
        host.close(root).await.expect("close session");
    }

    #[tokio::test]
    async fn private_same_name_under_different_origins_reads_only_own_facts() {
        let (host, root, child, _temp) = host_with_child().await;
        let user_caller = private_test_caller("focus", dal_core::Origin::User);
        let bundled_caller = private_test_caller("focus", dal_core::Origin::Bundled);
        let backend = backend_of(&host, root);
        let handle = backend.handle().clone();

        for (caller, tool) in [(&user_caller, "tool-a"), (&bundled_caller, "tool-b")] {
            let r#loop = dal_core::PrivateLoopId::new_v7();
            let token = dal_core::PrivateEffectId::new_v7();
            handle
                .private_loop_born(r#loop, private_owner_of(caller), None)
                .await
                .expect("loop birth receipts");
            handle
                .private_start(
                    r#loop,
                    token,
                    private_owner_of(caller),
                    None,
                    0,
                    dal_core::CallId::new("proof-call"),
                    tool.into(),
                    dal_core::RawJson::parse("null").expect("args parse"),
                )
                .await
                .expect("start receipts");
            handle
                .private_end(r#loop, token, private_settled_outcome())
                .await
                .expect("end receipts");
        }

        let user_rows = backend
            .private_outcomes(&user_caller)
            .await
            .expect("user reads its terminals");
        assert_eq!(user_rows.len(), 1, "user/focus reads only its terminal");
        assert_eq!(&*user_rows[0].name, "tool-a");
        let bundled_rows = backend
            .private_outcomes(&bundled_caller)
            .await
            .expect("bundled reads its terminals");
        assert_eq!(
            bundled_rows.len(),
            1,
            "the same name under bundled reads only its own"
        );
        assert_eq!(&*bundled_rows[0].name, "tool-b");
        let stranger = private_test_caller("stranger", dal_core::Origin::User);
        assert!(
            backend
                .private_outcomes(&stranger)
                .await
                .expect("stranger reads")
                .is_empty(),
            "a third owner reads nothing"
        );

        host.close(child).await.expect("close child");
        host.close(root).await.expect("close session");
    }

    #[tokio::test]
    async fn infer_deps_forwards_caller_continuation() {
        let (host, root, child, _temp) = host_with_child().await;
        let backend = backend_of(&host, root);

        let owner = crate::session::continuation::ContinuationOwner::new(
            crate::session::tasks::SessionTasks::new(),
        );
        let weak = owner.downgrade();
        let expected_slot = weak.upgrade().expect("owner is alive");

        let caller_with_slot =
            private_test_caller("focus", dal_core::Origin::User).with_continuation(weak.clone());
        let deps_with = backend.infer_deps(&caller_with_slot);
        let carried = deps_with
            .continuation
            .expect("continuation carried into deps");
        assert!(std::sync::Weak::ptr_eq(&carried, &weak));
        let carried_slot = carried.upgrade().expect("slot lives while owner lives");
        assert!(Arc::ptr_eq(&carried_slot, &expected_slot));

        let caller_without_slot = private_test_caller("focus", dal_core::Origin::User);
        let deps_without = backend.infer_deps(&caller_without_slot);
        assert!(
            deps_without.continuation.is_none(),
            "infer_deps must not invent a continuation for unattached caller"
        );

        host.close(child).await.expect("close child");
        host.close(root).await.expect("close session");
    }

    #[tokio::test]
    async fn infer_deps_for_stripped_setup_caller_runs_one_shot() {
        let (host, root, child, _temp) = host_with_child().await;
        let backend = backend_of(&host, root);

        let owner = crate::session::continuation::ContinuationOwner::new(
            crate::session::tasks::SessionTasks::new(),
        );
        let setup_caller = private_test_caller("focus", dal_core::Origin::User)
            .with_continuation(owner.downgrade());
        drop(owner);

        let retained = setup_caller.without_continuation();
        assert!(
            retained.continuation().is_none(),
            "a caller retained past setup must not carry the setup slot"
        );
        let deps = backend.infer_deps(&retained);
        assert!(
            deps.continuation.is_none(),
            "a stripped setup caller infers one-shot instead of failing on the released owner"
        );

        host.close(child).await.expect("close child");
        host.close(root).await.expect("close session");
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

    #[test]
    fn a_job_log_below_the_jobs_dir_resolves_and_a_missing_one_is_none() -> TestResult {
        let (_temp, root, _outside) = fixture()?;
        let jobs = root.join("jobs");
        std::fs::create_dir(&jobs)?;
        std::fs::write(jobs.join("a.log"), b"output")?;
        assert_eq!(
            resolve_job_log(&jobs, &jobs.join("a.log"))?,
            Some(std::fs::canonicalize(jobs.join("a.log"))?)
        );
        assert_eq!(resolve_job_log(&jobs, &jobs.join("absent.log"))?, None);
        let nowhere = root.join("never-created");
        assert_eq!(resolve_job_log(&nowhere, &nowhere.join("a.log"))?, None);
        Ok(())
    }

    #[test]
    fn only_paths_below_the_jobs_dir_are_served() -> TestResult {
        let (_temp, root, outside) = fixture()?;
        let jobs = root.join("jobs");
        std::fs::create_dir(&jobs)?;
        std::fs::write(outside.join("secret"), b"secret")?;
        std::fs::write(root.join("note.txt"), b"workspace file")?;
        for raw in [
            outside.join("secret"),
            root.join("note.txt"),
            jobs.clone(),
            jobs.join(".."),
            PathBuf::from("/etc/passwd"),
        ] {
            assert!(
                resolve_job_log(&jobs, &raw).is_err(),
                "{} must be refused",
                raw.display()
            );
        }
        assert_eq!(
            resolve_job_log(&jobs, &jobs.join("../outside/secret")),
            Err(ConfineError::ParentDir)
        );
        assert_eq!(
            resolve_job_log(&jobs, &jobs.join("x/../../note.txt")),
            Err(ConfineError::ParentDir)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_jobs_dir_is_refused() -> TestResult {
        let (_temp, root, outside) = fixture()?;
        let jobs = root.join("jobs");
        std::fs::create_dir(&jobs)?;
        std::fs::write(outside.join("secret"), b"secret")?;
        std::os::unix::fs::symlink(outside.join("secret"), jobs.join("leak.log"))?;
        std::os::unix::fs::symlink(&outside, jobs.join("dir"))?;
        assert_eq!(
            resolve_job_log(&jobs, &jobs.join("leak.log")),
            Err(ConfineError::Outside)
        );
        assert_eq!(
            resolve_job_log(&jobs, &jobs.join("dir/secret")),
            Err(ConfineError::Outside)
        );
        Ok(())
    }

    #[tokio::test]
    async fn read_job_log_serves_bytes_and_still_refuses_other_absolute_paths() -> TestResult {
        let (_temp, root, outside) = fixture()?;
        let jobs = root.join("jobs");
        std::fs::create_dir(&jobs)?;
        std::fs::write(jobs.join("a.log"), b"head\ntail\n")?;
        std::fs::write(outside.join("secret"), b"secret")?;
        let served = read_job_log(jobs.clone(), &jobs.join("a.log").to_string_lossy()).await?;
        assert_eq!(served.as_deref(), Some(b"head\ntail\n".as_slice()));
        let absent = read_job_log(jobs.clone(), &jobs.join("b.log").to_string_lossy()).await?;
        assert_eq!(absent, None);
        let refused = read_job_log(jobs, &outside.join("secret").to_string_lossy()).await;
        assert!(
            matches!(refused, Err(ServiceError::Denied(_))),
            "an absolute path outside the jobs dir stays denied: {refused:?}"
        );
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
