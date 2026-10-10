//! Session-scoped tool runtime behind extension services.
//!
//! [`SessionRt`] implements [`ToolCxRuntime`] for the capability gate: the
//! approval ladder runs over the session policy with no turn, so `Ask`
//! denies without a frontend exactly like turn-less direct calls. Spawns go
//! through the same admission and spawn door as call runtimes; detaches park
//! without turn-grant binding because no turn ledger exists at this scope.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use dal_core::{
    ApprovalMode, BlobId, CallId, DenyReason, EntryId, EntryKind, EntryView, JobId, JournalPart,
    Name, Policy, Preview, RunRequest, SessionId, ToolClass, Workspace,
};
use serde::Deserialize;
use tokio::sync::{Mutex, OwnedSemaphorePermit};
use tokio_util::sync::CancellationToken;

use crate::error::{SchemeError, ToolError};
use crate::ext::generation::Generation;
use crate::ext::scheme::{LetterSourceIndex, SchemeCx, SchemeCxRuntime, SchemeResolveContext};
use crate::ext::tool::{Approved, ToolCxRuntime};
use crate::ext::{BoxFuture, Caller, Doc};
use crate::host::HostState;
use crate::jobs::{JobRecord, JobTable};
use crate::proc::{Launcher, Proc, SpawnOpts, spawn_process};
use crate::session::dispatch::grant_covers;
use crate::session::service_grants::{CallKey, Covering};
use crate::session::tasks::SessionTasks;

/// Tool identity attributed to services `run` approvals.
#[expect(
    clippy::expect_used,
    reason = "\"run\" is a fixed literal that always satisfies the name grammar"
)]
pub(crate) fn service_tool() -> Name {
    Name::parse("run").expect("literal service tool name parses")
}

/// Class of the services `run` slot: an impure process execution.
pub(crate) fn service_class() -> ToolClass {
    ToolClass::Exec {
        read_only: false,
        grant: None,
    }
}

/// Construction inputs for the session-scoped run runtime.
pub(crate) struct SessionRtDeps {
    /// The session workspace.
    pub(crate) workspace: Workspace,
    pub(crate) shared: Arc<crate::session::shared::Shared>,
    pub(crate) initial_entries: Arc<[EntryView]>,
    pub(crate) scheme_store: Arc<dal_store::Store>,
    /// The host state for admission.
    pub(crate) host: Arc<HostState>,
    /// The session job table.
    pub(crate) jobs: Arc<Mutex<JobTable>>,
    /// Processes parked after turn-less service calls.
    pub(crate) procs: Arc<Mutex<HashMap<JobId, Proc>>>,
    /// The captured environment for launched processes.
    pub(crate) env_snapshot: Vec<(OsString, OsString)>,
    /// The process launcher prepared at session start; the error side
    /// refuses every spawn with the exact sandbox setup text.
    pub(crate) launcher: Result<Launcher, crate::proc::sandbox::SandboxSetupError>,
    /// The session approval mode.
    pub(crate) approval: ApprovalMode,
    /// The session cancellation token.
    pub(crate) cancel: CancellationToken,
    /// The durable job witness directory.
    pub(crate) jobs_dir: PathBuf,
    /// The shared owner for session background work.
    pub(crate) tasks: SessionTasks,
}

/// Session-scoped [`ToolCxRuntime`] owned by [`SessionServices`].
pub(crate) struct SessionRt {
    workspace: Workspace,
    host: Arc<HostState>,
    shared: Arc<crate::session::shared::Shared>,
    initial_entries: Arc<[EntryView]>,
    scheme_store: Arc<dal_store::Store>,
    jobs: Arc<Mutex<JobTable>>,
    procs: Arc<Mutex<HashMap<JobId, Proc>>>,
    env_snapshot: Vec<(OsString, OsString)>,
    launcher: Result<Launcher, crate::proc::sandbox::SandboxSetupError>,
    approval: ApprovalMode,
    cancel: CancellationToken,
    proofs: std::sync::Mutex<HashMap<CallId, SpawnProof>>,
    jobs_dir: PathBuf,
    tasks: SessionTasks,
}

impl SessionRt {
    /// Builds the session runtime from host-owned pieces.
    pub(crate) fn new(deps: SessionRtDeps) -> Self {
        let SessionRtDeps {
            workspace,
            shared,
            initial_entries,
            scheme_store,
            host,
            jobs,
            procs,
            env_snapshot,
            launcher,
            approval,
            cancel,
            jobs_dir,
            tasks,
        } = deps;
        Self {
            workspace,
            host,
            shared,
            initial_entries,
            scheme_store,
            jobs,
            procs,
            env_snapshot,
            launcher,
            approval,
            cancel,
            proofs: std::sync::Mutex::new(HashMap::new()),
            jobs_dir,
            tasks,
        }
    }

    /// The live grant that covers one `run` request `who` made, if any.
    async fn covering(&self, who: &Caller, req: &RunRequest) -> Option<Covering> {
        let key = CallKey::of(who)?;
        let cwd = req
            .cwd
            .as_deref()
            .unwrap_or_else(|| self.workspace.as_path());
        let jobs = self.jobs.lock().await;
        self.shared
            .service_grants()
            .cover(&key, &req.argv, cwd, &jobs)
    }

    /// Acquires the process permit for one allowed services run.
    async fn acquire(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(OwnedSemaphorePermit, crate::admission::FdPermit), DenyReason> {
        let process = self
            .host
            .shared
            .admission
            .acquire_process(cancel)
            .await
            .map_err(|error| match error {
                ToolError::Cancelled => DenyReason::Unavailable {
                    what: "turn cancelled".into(),
                },
                ToolError::Admission { limit } => DenyReason::Unavailable {
                    what: format!(
                        "no free {} slot within limits.admission_wait",
                        limit.as_str()
                    )
                    .into(),
                },
                _ => DenyReason::Unavailable {
                    what: "process unavailable".into(),
                },
            })?;
        let fds = self
            .host
            .shared
            .admission
            .charge_fds(crate::proc::PROCESS_FD_COST, cancel)
            .await
            .map_err(|error| match error {
                ToolError::Cancelled => DenyReason::Unavailable {
                    what: "turn cancelled".into(),
                },
                ToolError::Admission { limit } => DenyReason::Unavailable {
                    what: format!(
                        "no free {} slot within limits.admission_wait",
                        limit.as_str()
                    )
                    .into(),
                },
                _ => DenyReason::Unavailable {
                    what: "file descriptors unavailable".into(),
                },
            })?;
        Ok((process, fds))
    }
    /// Mints the spawn proof for one allowed services run: process and fd
    /// permits plus the digest-bound proof the spawn door consumes.
    async fn mint_allow(
        &self,
        call: &CallId,
        preview: Preview,
        cancel: &CancellationToken,
    ) -> Result<Approved, DenyReason> {
        let (permit, fds) = self.acquire(cancel).await?;
        let digest = preview.digest;
        self.proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(call.clone(), (digest, permit, fds, None));
        Ok(Approved::new(
            call.clone(),
            digest,
            Box::new([]),
            Box::new([self.workspace.as_path().to_path_buf()]),
            None,
        ))
    }
}

impl ToolCxRuntime for SessionRt {
    fn decide_run(&self) -> dal_core::Decision {
        let policy = Policy {
            mode: self.approval,
            answerer_attached: self.shared.attached_approval(),
            allow_always: self.shared.allow_always().as_ref().clone(),
        };
        policy.decide(&service_tool(), &service_class())
    }

    fn authorize(
        &self,
        call: &CallId,
        preview: Preview,
        cancel: &CancellationToken,
    ) -> BoxFuture<'_, Result<Approved, DenyReason>> {
        let call = call.clone();
        let cancel = cancel.clone();
        Box::pin(async move {
            match self.decide_run() {
                dal_core::Decision::Allow => self.mint_allow(&call, preview, &cancel).await,
                dal_core::Decision::Deny { reason } => Err(match reason {
                    DenyReason::NoFrontEnd => DenyReason::out_of_scope(
                        dal_core::headless_denial_text("run", dal_core::rung(&service_class())),
                    ),
                    reason => reason,
                }),
                // The services layer routes `Ask` through the session
                // broker; a direct authorize has no question to open.
                dal_core::Decision::Ask { .. } => Err(DenyReason::NoFrontEnd),
                _ => Err(DenyReason::out_of_scope(service_tool().as_str())),
            }
        })
    }

    fn authorize_approved(
        &self,
        call: &CallId,
        preview: Preview,
        cancel: &CancellationToken,
    ) -> BoxFuture<'_, Result<Approved, DenyReason>> {
        let call = call.clone();
        let cancel = cancel.clone();
        Box::pin(async move { self.mint_allow(&call, preview, &cancel).await })
    }

    fn covered_run<'a>(
        &'a self,
        who: &'a Caller,
        call: &'a CallId,
        req: &'a RunRequest,
        preview: &'a Preview,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Option<Approved>, DenyReason>> {
        Box::pin(async move {
            if self.covering(who, req).await.is_none() {
                return Ok(None);
            }
            let (permit, fds) = self.acquire(cancel).await?;
            // The wait for a slot may outlast the run that earned the grant.
            let Some(covering) = self.covering(who, req).await else {
                return Err(DenyReason::NotGranted);
            };
            let digest = preview.digest;
            self.proofs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(call.clone(), (digest, permit, fds, CallKey::of(who)));
            Ok(Some(Approved::new(
                call.clone(),
                digest,
                covering.prefix,
                covering.roots,
                covering.job,
            )))
        })
    }

    fn job_started(&self, who: &Caller, job: JobId) {
        if let Some(key) = CallKey::of(who) {
            self.shared.service_grants().bind_job(&key, job);
        }
    }

    fn spawn(
        &self,
        argv: &[OsString],
        opts: SpawnOpts,
        approved: Approved,
    ) -> Result<Proc, ToolError> {
        let proof = self
            .proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(approved.call());
        let Some((digest, permit, fd_permit, key)) = proof else {
            return Err(ToolError::Denied(DenyReason::NotGranted));
        };
        if let Some(key) = key {
            let Ok(jobs) = self.jobs.try_lock() else {
                return Err(ToolError::Denied(DenyReason::NotGranted));
            };
            if approved.job().is_some_and(|job| !jobs.is_live(job))
                || !self.shared.service_grants().is_live(&key, &jobs)
            {
                return Err(ToolError::Denied(DenyReason::NotGranted));
            }
        }
        let launcher = match &self.launcher {
            Ok(launcher) => launcher,
            Err(setup) => return Err(setup.tool_error()),
        };
        if !approved.prefix().is_empty() && !grant_covers(&approved, argv, &opts.cwd) {
            return Err(ToolError::Denied(DenyReason::out_of_scope(
                service_tool().as_str(),
            )));
        }
        spawn_process(
            argv,
            approved.call().clone(),
            opts,
            &approved,
            digest,
            &self.workspace,
            &self.jobs_dir,
            &self.env_snapshot,
            launcher,
            permit,
            Some(fd_permit),
        )
    }

    fn detach(&self, proc: Proc) -> JobId {
        let job = proc.job_id();
        let cancel = CancellationToken::new();
        let record = JobRecord::new(
            job,
            "service",
            proc.log_path().to_path_buf(),
            cancel.clone(),
        );
        let jobs = Arc::clone(&self.jobs);
        let procs = Arc::clone(&self.procs);
        self.tasks.spawn(async move {
            let mut table = jobs.lock().await;
            if table.reserve(record).is_err() || table.adopt_detached(job).is_err() {
                return;
            }
            drop(table);
            let proc = {
                let mut procs = procs.lock().await;
                procs.insert(job, proc);
                procs.remove(&job)
            };
            if let Some(proc) = proc {
                crate::jobs::reap_detached(proc, jobs, cancel).await;
            }
        });
        job
    }

    fn resolve(
        &self,
        uri: &str,
        context: SchemeResolveContext<'_>,
    ) -> BoxFuture<'_, Result<Doc, ToolError>> {
        let uri: Box<str> = uri.into();
        let jobs = Arc::clone(&self.jobs);
        if uri.starts_with("job://") {
            return Box::pin(async move {
                let table = jobs.lock().await;
                table.read_uri(&uri)
            });
        }
        let generation = self.host.shared.generation.borrow().clone();
        let store = Arc::clone(&self.scheme_store);
        let shared = Arc::clone(&self.shared);
        let initial_entries = Arc::clone(&self.initial_entries);
        crate::session::rt::resolve_extension_scheme(uri, &generation, &context, move || {
            (store, shared, initial_entries)
        })
    }

    fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }
}

struct SchemeRuntime {
    store: Arc<dal_store::Store>,
    shared: Arc<crate::session::shared::Shared>,
    initial_entries: Arc<[EntryView]>,
    session: SessionId,
}

#[derive(Deserialize)]
struct HistoryLetterRecord {
    kind: Box<str>,
    id: Box<str>,
    #[serde(default)]
    spans: Vec<HistorySpan>,
}

#[derive(Deserialize)]
struct HistorySpan(u64, u32, u32, u32);

fn history_unavailable() -> SchemeError {
    SchemeError::Failed {
        message: "letter history index is unavailable".into(),
    }
}

fn history_record(body: &dal_core::RawJson) -> Option<HistoryLetterRecord> {
    sonic_rs::from_str(body.as_str()).ok()
}

fn history_rows(shared: &crate::session::shared::Shared) -> Vec<HistoryLetterRecord> {
    shared
        .ext_records()
        .iter()
        .filter(|record| record.kind.as_ref() == "letter")
        .filter_map(|record| history_record(&record.body))
        .filter(|record| {
            record.kind.as_ref() == "compaction"
                && record.id.starts_with("history/")
                && !record.spans.is_empty()
        })
        .collect()
}

fn history_entries(
    initial: &[EntryView],
    shared: &crate::session::shared::Shared,
) -> BTreeMap<EntryId, EntryView> {
    let mut entries: BTreeMap<EntryId, EntryView> = initial
        .iter()
        .cloned()
        .map(|entry| (entry.id, entry))
        .collect();
    entries.extend(
        shared
            .leaf_entries()
            .into_iter()
            .map(|entry| (entry.id, entry)),
    );
    entries
}

fn history_part_text(
    part: &JournalPart,
    store: &dal_store::Store,
    session: SessionId,
) -> Result<Option<String>, SchemeError> {
    match part {
        JournalPart::Text { text } => Ok(Some(text.to_string())),
        JournalPart::TextBlob { blob, .. } => {
            let id = BlobId::parse(blob).map_err(|error| SchemeError::Failed {
                message: error.to_string().into(),
            })?;
            let bytes = store.read_blob(session, id).map_err(SchemeError::from)?;
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|error| SchemeError::Failed {
                    message: error.to_string().into(),
                })
        }
        JournalPart::Blob { mime, blob, .. } if mime.starts_with("image/") => Ok(None),
        JournalPart::Blob { blob, .. } => {
            let id = BlobId::parse(blob).map_err(|error| SchemeError::Failed {
                message: error.to_string().into(),
            })?;
            let bytes = store.read_blob(session, id).map_err(SchemeError::from)?;
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|error| SchemeError::Failed {
                    message: error.to_string().into(),
                })
        }
        JournalPart::Image { .. } | JournalPart::ImageBlob { .. } => Ok(None),
    }
}

/// Resolves a history span's `(role, full text)` for its entry kind, or
/// `None` when the part holds a non-text payload.
fn history_span_role_text(
    entry: &EntryView,
    part_index: usize,
    store: &dal_store::Store,
    session: SessionId,
) -> Result<Option<(String, String)>, SchemeError> {
    let part_text = |part: &JournalPart| history_part_text(part, store, session);
    let (role, text) = match &entry.kind {
        EntryKind::User { parts } | EntryKind::Compaction { parts, .. } => {
            let part = parts.get(part_index).ok_or_else(|| SchemeError::Failed {
                message: format!(
                    "history span refers to a missing part in entry {}",
                    entry.id
                )
                .into(),
            })?;
            let Some(text) = part_text(part)? else {
                return Ok(None);
            };
            ("user".to_owned(), text)
        }
        EntryKind::ToolResult {
            name, error, parts, ..
        } => {
            let part = parts.get(part_index).ok_or_else(|| SchemeError::Failed {
                message: format!(
                    "history span refers to a missing part in entry {}",
                    entry.id
                )
                .into(),
            })?;
            let Some(text) = part_text(part)? else {
                return Ok(None);
            };
            let role = if *error { "failed output" } else { "output" };
            (format!("{role} {name}"), text)
        }
        EntryKind::Assistant { content, .. } => {
            let (role, text) = match content.get(part_index) {
                Some(dal_core::Block::Text { text }) => ("assistant".to_owned(), text.to_string()),
                Some(dal_core::Block::Reasoning { text, .. }) => {
                    ("reasoning".to_owned(), text.to_string())
                }
                Some(dal_core::Block::ToolCall { name, input, .. }) => {
                    (format!("call {name}"), input.as_str().to_owned())
                }
                None => {
                    return Err(SchemeError::Failed {
                        message: format!(
                            "history span refers to a missing block in entry {}",
                            entry.id
                        )
                        .into(),
                    });
                }
            };
            (role, text)
        }
        EntryKind::Reminder { text, .. } if part_index == 0 => {
            ("note".to_owned(), text.to_string())
        }
        _ => {
            return Err(SchemeError::Failed {
                message: format!("history span refers to a non-text entry {}", entry.id).into(),
            });
        }
    };
    Ok(Some((role, text)))
}

fn history_span_text(
    entry: &EntryView,
    span: &HistorySpan,
    store: &dal_store::Store,
    session: SessionId,
) -> Result<Option<String>, SchemeError> {
    let part_index = usize::try_from(span.1).map_err(|error| SchemeError::Failed {
        message: error.to_string().into(),
    })?;
    let Some((role, text)) = history_span_role_text(entry, part_index, store, session)? else {
        return Ok(None);
    };
    let start = usize::try_from(span.2).map_err(|error| SchemeError::Failed {
        message: error.to_string().into(),
    })?;
    let end_value = span
        .2
        .checked_add(span.3)
        .ok_or_else(|| SchemeError::Failed {
            message: "history span byte range is invalid".into(),
        })?;
    let end = usize::try_from(end_value).map_err(|error| SchemeError::Failed {
        message: error.to_string().into(),
    })?;
    let slice = text.get(start..end).ok_or_else(|| SchemeError::Failed {
        message: format!("history span byte range is invalid for entry {}", entry.id).into(),
    })?;
    let total = text.len();
    let mut piece = format!(
        "=== entry {}, {}, part {}, bytes {}-{end} of {total}\n{slice}",
        entry.id, role, span.1, span.2
    );
    if !piece.ends_with('\n') {
        piece.push('\n');
    }
    Ok(Some(piece))
}

fn read_history_source(
    store: &dal_store::Store,
    session: SessionId,
    entries: &BTreeMap<EntryId, EntryView>,
    id: &str,
    record: &HistoryLetterRecord,
) -> Result<Arc<[u8]>, SchemeError> {
    let mut pieces = Vec::with_capacity(record.spans.len());
    for span in &record.spans {
        let entry_id = std::num::NonZeroU64::new(span.0)
            .map(EntryId::new)
            .ok_or_else(|| SchemeError::Failed {
                message: "history span entry id is invalid".into(),
            })?;
        let entry = entries.get(&entry_id).ok_or_else(|| SchemeError::Failed {
            message: format!("history source entry {entry_id} is not on this session branch")
                .into(),
        })?;
        if let Some(piece) = history_span_text(entry, span, store, session)? {
            pieces.push(piece);
        }
    }
    let noun = if pieces.len() == 1 { "piece" } else { "pieces" };
    let text = format!(
        "letter://{id} holds {} {noun} of the journal of session {session}. Each piece starts with a line that begins with \"=== entry\". The exact text of the piece follows that line, and a line break ends a piece whose text does not end with one. \"bytes a-b\" means from byte a up to, but not including, byte b.\n{}",
        pieces.len(),
        pieces.concat()
    );
    Ok(Arc::from(text.into_bytes()))
}

impl LetterSourceIndex for SchemeRuntime {
    fn index_lines(&self, session: SessionId) -> BoxFuture<'_, Result<Vec<Box<str>>, SchemeError>> {
        Box::pin(async move {
            if session != self.session {
                return Err(history_unavailable());
            }
            Ok(history_rows(&self.shared)
                .into_iter()
                .filter_map(|record| {
                    let first = record.spans.iter().map(|span| span.0).min()?;
                    let last = record.spans.iter().map(|span| span.0).max()?;
                    Some(
                        format!(
                            "letter://{}  history image, entries {first}-{last}",
                            record.id
                        )
                        .into_boxed_str(),
                    )
                })
                .collect())
        })
    }

    fn read<'a>(
        &'a self,
        session: SessionId,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<[u8]>, SchemeError>> {
        let store = Arc::clone(&self.store);
        let entries = history_entries(&self.initial_entries, &self.shared);
        let record = history_rows(&self.shared)
            .into_iter()
            .find(|record| record.id.as_ref() == id);
        let id = id.to_owned();
        Box::pin(async move {
            if session != self.session {
                return Err(history_unavailable());
            }
            let record = record.ok_or_else(|| SchemeError::Failed {
                message: format!("letter {id} does not exist in this session").into(),
            })?;
            tokio::task::spawn_blocking(move || {
                read_history_source(&store, session, &entries, &id, &record)
            })
            .await
            .map_err(|error| SchemeError::Failed {
                message: format!("letter history read task failed: {error}").into(),
            })?
        })
    }
}

impl SchemeCxRuntime for SchemeRuntime {
    fn blob_get<'a>(
        &'a self,
        id: &'a BlobId,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, SchemeError>> {
        let store = Arc::clone(&self.store);
        let session = self.session;
        let id = *id;
        Box::pin(async move {
            let result = tokio::task::spawn_blocking(move || store.read_blob(session, id))
                .await
                .map_err(|error| SchemeError::Failed {
                    message: format!("blob read task failed: {error}").into(),
                })?;
            match result {
                Ok(bytes) => Ok(Some(bytes)),
                Err(dal_store::BlobError::NotFound { .. }) => Ok(None),
                Err(error) => Err(SchemeError::from(error)),
            }
        })
    }

    fn letter_index(&self) -> &dyn LetterSourceIndex {
        self
    }
}

/// One minted spawn proof: the bound digest, the process slot, and the fd
/// permit released when the child settles.
type SpawnProof = (
    Option<[u8; 32]>,
    OwnedSemaphorePermit,
    crate::admission::FdPermit,
    Option<CallKey>,
);

pub(crate) fn resolve_extension_scheme(
    uri: Box<str>,
    generation: &Generation,
    context: &SchemeResolveContext<'_>,
    make_store: impl FnOnce() -> (
        Arc<dal_store::Store>,
        Arc<crate::session::shared::Shared>,
        Arc<[EntryView]>,
    ),
) -> BoxFuture<'static, Result<Doc, ToolError>> {
    let Some((scheme, _path)) = uri.split_once("://") else {
        return Box::pin(async move { Err(ToolError::Scheme(SchemeError::NotFound { uri })) });
    };
    let Some(resolver) = generation.scheme(scheme) else {
        return Box::pin(async move { Err(ToolError::Scheme(SchemeError::NotFound { uri })) });
    };
    let path_start = scheme.len() + "://".len();
    let resolver = Arc::clone(resolver);
    let caller = (*context.caller).clone();
    let services = Arc::clone(context.services);
    let session = context.session;
    let (store, shared, initial_entries) = make_store();
    let cx = SchemeCx::new(
        caller,
        services,
        session,
        Arc::new(SchemeRuntime {
            store,
            shared,
            initial_entries,
            session,
        }),
        Arc::clone(&generation.docs),
    );
    Box::pin(async move {
        resolver
            .read(&uri[path_start..], &cx)
            .await
            .map_err(ToolError::Scheme)
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use dal_core::{
        Entry, EntryId, EntryKind, Gen, Product, RawJson, Record, Session, SessionId,
        ThinkingLevel, Workspace,
    };
    use dal_store::Store;

    use super::*;

    #[tokio::test]
    async fn letter_history_resolves_sources_from_a_reopened_journal() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let workspace = Workspace::new(temp.path().join("workspace")).expect("workspace");
        let session = SessionId::new_v7();
        let store = Arc::new(Store::new(
            temp.path().join("data"),
            workspace,
            Product::Dal,
        ));
        let mut journal = store.create_session(session);
        journal
            .append(vec![Record::User(Entry {
                id: EntryId::new(NonZeroU64::MIN),
                parent: None,
                at: jiff::Timestamp::UNIX_EPOCH,
                kind: EntryKind::User {
                    parts: vec![
                        JournalPart::Text {
                            text: "hello".into(),
                        },
                        JournalPart::Image {
                            mime: "image/png".into(),
                            base64: "AQID".into(),
                        },
                    ],
                },
            })])
            .await
            .expect("write source entry");
        journal
            .append(vec![Record::Ext {
                at: jiff::Timestamp::UNIX_EPOCH,
                ext: "history".into(),
                kind: "letter".into(),
                body: RawJson::parse(
                    r#"{"v":1,"id":"history/1.1","kind":"compaction","png_blob":"0000000000000000000000000000000000000000000000000000000000000000","png_bytes":1,"width":1,"height":1,"cell":[1,1],"spans":[[1,0,0,5],[1,1,0,3]],"letters":[]}"#,
                )
                .expect("history body"),
            }])
            .await
            .expect("write history record");
        journal.close().await.expect("close initial journal");
        drop(journal);

        let (mut reopened, _) = store.open_session(session).await.expect("reopen journal");
        let (fold, _) = Session::replay(reopened.records().to_vec(), jiff::Timestamp::UNIX_EPOCH)
            .expect("replay reopened journal");
        let entries: Arc<[EntryView]> = fold
            .leaf_entries()
            .into_iter()
            .map(|entry| EntryView {
                id: entry.id,
                parent: entry.parent,
                kind: entry.kind.clone(),
            })
            .collect::<Vec<_>>()
            .into();
        let shared = Arc::new(crate::session::shared::Shared::new(
            false,
            Gen::new(NonZeroU64::MIN),
            ThinkingLevel::Medium,
            ApprovalMode::Ask,
            dal_core::Mode::Normal,
        ));
        shared.sync_ext(&fold);
        let runtime = SchemeRuntime {
            store: Arc::clone(&store),
            shared,
            initial_entries: entries,
            session,
        };

        let lines = runtime.index_lines(session).await.expect("history index");
        let expected: Vec<Box<str>> =
            vec!["letter://history/1.1  history image, entries 1-1".into()];
        assert_eq!(lines, expected);
        let source = runtime
            .read(session, "history/1.1")
            .await
            .expect("history source");
        let source = String::from_utf8(source.to_vec()).expect("source is UTF-8");
        assert!(source.contains("=== entry 1, user, part 0, bytes 0-5 of 5\nhello"));
        assert!(matches!(
            runtime.read(SessionId::new_v7(), "history/1.1").await,
            Err(SchemeError::Failed { .. })
        ));
        reopened.close().await.expect("close reopened journal");
    }
}
