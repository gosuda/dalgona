//! Host backing for extension command contexts.
//!
//! [`DriverHost`] implements [`CommandHost`] over per-command snapshots:
//! the session view, hydrated leaf entries, journal paths, and the active
//! model scope are captured once in [`driver_host`] so the sync trait
//! methods serve clones without further round-trips.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

use dal_core::{
    ApprovalMode, ClientId, Command, EntryKind, EntryView, JobId, JobOutcome, JournalPart,
    ListQuery, Mode, ModelRoute, Page, PageReq, Save, SessionId, SessionSummary, ThinkingLevel,
    TurnId, View, Workspace,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::broker::Broker;
use crate::error::AgentError;
use crate::ext::command::{CatalogView, CommandHost, ResolveMiss, SaveError};
use crate::ext::{BoxFuture, Caller};
use crate::host::HostState;
use crate::jobs::{JobRecord, JobTable};
use crate::session::driver::DriverDeps;
use crate::session::projection::SnapshotArgs;
use crate::session::shared::Shared;
use crate::session::tasks::SessionTasks;
use crate::session::{SessionHandle, TurnRequest};

/// Default rows for a command session listing without an explicit limit.
const SESSIONS_DEFAULT: u32 = 50;

/// Leaf entries handed to one command handler; clamped to the largest page the view allows.
const LEAF_LIMIT: u32 = 1_024;

/// Builds the leaf page: the clamp keeps [`LEAF_LIMIT`] within
/// [`PageReq::MAX_LIMIT`], so the maximum check cannot fail and the fallback
/// only satisfies the result type.
fn leaf_page() -> PageReq {
    leaf_page_before(None)
}

fn leaf_page_before(before: Option<dal_core::EntryId>) -> PageReq {
    let limit = match NonZeroU32::new(LEAF_LIMIT.min(PageReq::MAX_LIMIT)) {
        Some(limit) => limit,
        None => NonZeroU32::MIN,
    };
    PageReq::new(limit, before).unwrap_or_default()
}

/// Host backing for [`CommandCx`](crate::ext::command::CommandCx); snapshots per command invocation.
#[derive(Clone)]
pub(crate) struct DriverHost {
    session: SessionId,
    workspace: Workspace,
    handle: SessionHandle,
    host: Arc<HostState>,
    broker: Arc<Broker>,
    jobs: Arc<Mutex<JobTable>>,
    shared: Arc<Shared>,
    generation: dal_core::Gen,
    pub(super) view: View,
    data_root: PathBuf,
    session_file: Option<PathBuf>,
    log_path: PathBuf,
    scoped: Option<Vec<Box<str>>>,
    route: Option<ModelRoute>,
    tasks: SessionTasks,
    pub(crate) leaf: Vec<EntryView>,
}

/// Snapshots one command host from live session state.
pub(crate) async fn driver_host(
    deps: &DriverDeps,
    scoped: Option<&[Box<str>]>,
    route: Option<ModelRoute>,
) -> DriverHost {
    let view = deps.shared.snapshot(SnapshotArgs {
        generation: deps.generation,
        id: deps.session,
        workspace: deps.workspace.clone(),
        open: deps.broker.open_requests(),
        updated_at: dal_core::Timestamp::now(),
        created_at: None,
        archived: None,
        page: leaf_page(),
    });
    let data_root = deps.host.shared.data_root.clone();
    let store = dal_store::Store::new(
        data_root.clone(),
        deps.workspace.clone(),
        journal_product(deps.host.shared.product_name),
    );
    let session_file = if deps.ephemeral {
        None
    } else {
        Some(store.session_file(deps.session))
    };
    let log_path = session_file.clone().unwrap_or_else(|| data_root.clone());
    let leaf = hydrate(&deps.handle, &view.entries.items).await;
    DriverHost {
        session: deps.session,
        workspace: deps.workspace.clone(),
        handle: deps.handle.clone(),
        host: Arc::clone(&deps.host),
        broker: Arc::clone(&deps.broker),
        jobs: Arc::clone(&deps.jobs),
        shared: Arc::clone(&deps.shared),
        generation: deps.generation,
        view,
        data_root,
        session_file,
        log_path,
        scoped: scoped.map(<[Box<str>]>::to_vec),
        route,
        tasks: deps.tasks.clone(),
        leaf,
    }
}

/// Hydrates text blobs in leaf entries through the session journal.
async fn hydrate(handle: &SessionHandle, items: &[EntryView]) -> Vec<EntryView> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(hydrate_entry(handle, item).await);
    }
    out
}

/// Hydrates one entry's text blobs, keeping the original part on failure.
async fn hydrate_entry(handle: &SessionHandle, item: &EntryView) -> EntryView {
    let mut entry = item.clone();
    let (EntryKind::User { parts } | EntryKind::ToolResult { parts, .. }) = &mut entry.kind else {
        return entry;
    };
    for part in parts.iter_mut() {
        let JournalPart::TextBlob { blob, .. } = part else {
            continue;
        };
        let Ok(id) = dal_core::BlobId::parse(blob) else {
            continue;
        };
        let Ok(bytes) = handle.blob(id).await else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        *part = JournalPart::Text { text: text.into() };
    }
    entry
}

/// Maps the host product name to its journal product.
pub(crate) fn journal_product(name: &str) -> dal_core::Product {
    if name == "dalgona" {
        dal_core::Product::Dalgona
    } else {
        dal_core::Product::Dal
    }
}
/// Returns the store session `jobs/` directory for process witnesses.
pub(crate) fn session_jobs_dir(
    host: &HostState,
    workspace: &Workspace,
    session: SessionId,
) -> PathBuf {
    let store = dal_store::Store::new(
        host.shared.data_root.clone(),
        workspace.clone(),
        journal_product(host.shared.product_name),
    );
    store.session_jobs_dir(session)
}

/// Writes one export payload to its durable job log, creating the jobs
/// directory when needed. A failure is surfaced by the caller as a failed
/// job outcome rather than an empty successful log.
async fn export_log(text: &str, log_path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = log_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(log_path, text.as_bytes()).await
}

/// Maps one durable job-log write to its outcome: a written log is an
/// exit-0 job; an unwritten one fails naming the job label, the target,
/// and the io error.
async fn job_log_outcome(label: &str, text: &str, log_path: &std::path::Path) -> JobOutcome {
    match export_log(text, log_path).await {
        Ok(()) => JobOutcome::Exited { code: 0 },
        Err(error) => JobOutcome::Failed {
            message: format!(
                "{label} log {} failed to write: {error}",
                log_path.display()
            )
            .into(),
        },
    }
}

/// Maps a failed actor operation to its typed save outcome.
///
/// `SessionClosed` keeps its identity. Post-apply persistence failures
/// arrive as [`AgentError::PersistenceFailed`] and become
/// [`SaveError::Failed`] with the message preserved; the message names
/// the failed persistence (journal today, distinctly-worded default
/// writes in future). Every other [`AgentError`] is a pre-apply
/// rejection that changed nothing, so it becomes
/// [`SaveError::Rejected`].
fn save_error(error: AgentError) -> SaveError {
    match error {
        AgentError::SessionClosed { id } => SaveError::SessionClosed { id },
        AgentError::PersistenceFailed { message } => SaveError::Failed { message },
        other => SaveError::Rejected {
            message: other.to_string().into(),
        },
    }
}

/// Labels one command for job table rows.
fn command_label(cmd: &Command) -> String {
    match cmd {
        Command::Run { name, .. } => format!("run:{name}"),
        Command::Export { .. } => String::from("export"),
        Command::Clone => String::from("clone"),
        Command::Fork(_) => String::from("fork"),
        Command::ReloadPlugins => String::from("reload"),
        Command::SetScopedModels(_) => String::from("models"),
        Command::Cancel { .. } => String::from("cancel"),
        _ => String::from("command"),
    }
}

enum PersistDefault {
    Model(ModelRoute),
    Thinking(ThinkingLevel),
    Approval(ApprovalMode),
    Mode(Mode),
}

fn default_model_text(route: &ModelRoute) -> String {
    match route {
        ModelRoute::Api {
            provider: Some(provider),
            model,
            ..
        } => format!("{provider}/{model}"),
        other => other.family_qualified_reference(),
    }
}

async fn persist_default(
    host: &Arc<crate::host::HostState>,
    persist: PersistDefault,
) -> Result<(), SaveError> {
    let _guard = host.shared.config_write.lock().await;
    let path = host
        .shared
        .config_path
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let Some(path) = path else {
        return Err(SaveError::Failed {
            message: "cannot save dal.toml: no config path".into(),
        });
    };
    let user_toml = match tokio::fs::read_to_string(&path).await {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(SaveError::Failed {
                message: format!("cannot read dal.toml: {error}").into(),
            });
        }
    };
    let mut config = host.shared.config.clone();
    let updated = match persist {
        PersistDefault::Model(route) => {
            let text = default_model_text(&route);
            if text.is_empty() {
                return Err(SaveError::Failed {
                    message: "cannot update dal.toml: empty model reference".into(),
                });
            }
            config
                .update_default_model(&text, user_toml.as_deref())
                .map_err(|error| SaveError::Failed {
                    message: format!("cannot update dal.toml: {error}").into(),
                })?
        }
        PersistDefault::Thinking(level) => config
            .update_default_thinking(level, user_toml.as_deref())
            .map_err(|error| SaveError::Failed {
                message: format!("cannot update dal.toml: {error}").into(),
            })?,
        PersistDefault::Approval(mode) => config
            .update_default_approval(mode, user_toml.as_deref())
            .map_err(|error| SaveError::Failed {
                message: format!("cannot update dal.toml: {error}").into(),
            })?,
        PersistDefault::Mode(mode) => config
            .update_default_mode(mode, user_toml.as_deref())
            .map_err(|error| SaveError::Failed {
                message: format!("cannot update dal.toml: {error}").into(),
            })?,
    };
    let bytes = updated.into_bytes();
    let parent = path.parent().map(std::path::Path::to_path_buf);
    let write_path = path.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        if let Some(parent) = parent
            && let Err(error) = dal_store::create_private_dir_all(&parent)
        {
            return Err(format!("cannot create {}: {error}", parent.display()));
        }
        dal_store::write_atomic(&write_path, &bytes, dal_store::FileMode::Mode0600)
            .map_err(|error| format!("cannot save {}: {error}", write_path.display()))
    })
    .await;
    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(reason)) => Err(SaveError::Failed {
            message: reason.into(),
        }),
        Err(error) => Err(SaveError::Failed {
            message: format!("cannot save {}: {error}", path.display()).into(),
        }),
    }
}

fn persist_for(command: &Command) -> Option<PersistDefault> {
    match command {
        Command::SetModel {
            model,
            save: Save::SessionAndDefault,
        } => Some(PersistDefault::Model(model.clone())),
        Command::SetThinking {
            level,
            save: Save::SessionAndDefault,
        } => Some(PersistDefault::Thinking(*level)),
        Command::SetApproval {
            mode,
            save: Save::SessionAndDefault,
        } => Some(PersistDefault::Approval(*mode)),
        Command::SetMode {
            mode,
            save: Save::SessionAndDefault,
        } => Some(PersistDefault::Mode(*mode)),
        _ => None,
    }
}

impl CommandHost for DriverHost {
    fn view(&self, _caller: &Caller, _session: SessionId, _turn: Option<TurnId>) -> View {
        self.view.clone()
    }

    fn data_root(&self, _caller: &Caller, _session: SessionId, _turn: Option<TurnId>) -> PathBuf {
        self.data_root.clone()
    }

    fn session_file(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
    ) -> Option<PathBuf> {
        self.session_file.clone()
    }

    fn log_path(&self, _caller: &Caller, _session: SessionId, _turn: Option<TurnId>) -> PathBuf {
        self.log_path.clone()
    }

    fn submit_wait(
        &self,
        caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
        command: Command,
    ) -> BoxFuture<'_, Result<(), SaveError>> {
        if super::untrusted_export_path_denied(caller.origin(), &command) {
            return Box::pin(async {
                Err(SaveError::Rejected {
                    message: super::EXPORT_PATH_DENIED.into(),
                })
            });
        }
        let persist = persist_for(&command);
        let handle = self.handle.clone();
        let host = Arc::clone(&self.host);
        let by = ClientId::new(caller.ext().as_str());
        Box::pin(async move {
            handle
                .submit(command, by)
                .await
                .map(|_| ())
                .map_err(save_error)?;
            if let Some(persist) = persist {
                persist_default(&host, persist).await?;
            }
            Ok(())
        })
    }

    fn start_job(
        &self,
        caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
        command: Command,
    ) -> JobId {
        let denied_export_path = super::untrusted_export_path_denied(caller.origin(), &command);
        let id = JobId::new_v7();
        let handle = self.handle.clone();
        let jobs = Arc::clone(&self.jobs);
        let by = ClientId::new(caller.ext().as_str());
        let label = command_label(&command);
        let log_path =
            session_jobs_dir(&self.host, &self.workspace, self.session).join(format!("{id:?}.log"));
        let record = JobRecord::new(
            id,
            label.clone(),
            log_path.clone(),
            CancellationToken::new(),
        );
        self.tasks.spawn(async move {
            {
                let mut table = jobs.lock().await;
                if table.reserve(record).is_err() {
                    return;
                }
                let _ = table.mark_running(id);
            }
            let outcome = if denied_export_path {
                JobOutcome::Failed {
                    message: super::EXPORT_PATH_DENIED.into(),
                }
            } else {
                match handle.submit(command, by).await {
                    Ok(reply) => match sonic_rs::to_string(&reply) {
                        Err(error) => JobOutcome::Failed {
                            message: format!("{label} reply failed to serialize: {error}").into(),
                        },
                        Ok(text) => job_log_outcome(&label, &text, &log_path).await,
                    },
                    Err(error) => JobOutcome::Failed {
                        message: error.to_string().into(),
                    },
                }
            };
            let tail = tokio::fs::read(&log_path)
                .await
                .unwrap_or_default()
                .into_boxed_slice();
            if let Err(error) = jobs.lock().await.settle_durably(id, outcome, tail).await {
                tracing::warn!(target: "dalgon.agent", job = %id, error = %error, "job ledger flush failed");
            }
        });
        id
    }

    fn cancel_turn(&self, _caller: &Caller, _session: SessionId, turn: Option<TurnId>) {
        let Some(_turn) = turn else {
            return;
        };
        let handle = self.handle.clone();
        self.tasks.spawn(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = handle
                .turn(TurnRequest {
                    op: dal_core::TurnOp::Cancel,
                    reply: tx,
                })
                .await;
            let _ = rx.await;
        });
    }

    fn sessions_page(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
        limit: u32,
        cursor: Option<&str>,
        search: Option<&str>,
    ) -> Page<SessionSummary, Box<str>> {
        let host = crate::host::Host {
            state: Arc::clone(&self.host),
        };
        let rows = host
            .sessions(ListQuery {
                limit: None,
                cursor: None,
                search: search.map(Into::into),
            })
            .map(|page| page.items)
            .unwrap_or_default();
        let counts = live_counts(&self.host);
        let mut items: Vec<SessionSummary> = rows
            .into_iter()
            .filter(|info| {
                search.is_none_or(|query| {
                    info.id.to_string().contains(query)
                        || info.name.as_ref().is_some_and(|name| name.contains(query))
                })
            })
            .map(|info| {
                let message_count = live_or_stored_count(&counts, &info);
                SessionSummary {
                    id: info.id,
                    name: info.name.clone(),
                    message_count,
                    updated_at: info.updated_at,
                }
            })
            .collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        let offset = cursor
            .and_then(|text| text.parse::<usize>().ok())
            .unwrap_or(0);
        let limit = if limit == 0 {
            SESSIONS_DEFAULT as usize
        } else {
            limit as usize
        };
        let page: Vec<SessionSummary> = items.into_iter().skip(offset).take(limit).collect();
        let next = if page.len() == limit {
            Some((offset + limit).to_string().into_boxed_str())
        } else {
            None
        };
        Page {
            items: page,
            next_before: next,
        }
    }

    fn resolve_session(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
        query: &str,
    ) -> Result<SessionSummary, ResolveMiss> {
        let host = crate::host::Host {
            state: Arc::clone(&self.host),
        };
        let rows = host
            .sessions(ListQuery {
                limit: None,
                cursor: None,
                search: None,
            })
            .map(|page| page.items)
            .unwrap_or_default();
        let counts = live_counts(&self.host);
        if let Some(info) = rows
            .iter()
            .find(|info| info.id.to_string().starts_with(query))
        {
            return Ok(summarize_row(info, live_or_stored_count(&counts, info)));
        }
        if let Some(info) = rows.iter().find(|info| {
            info.name
                .as_ref()
                .is_some_and(|name| name.as_ref() == query)
        }) {
            return Ok(summarize_row(info, live_or_stored_count(&counts, info)));
        }
        // The listing above is capped per workspace, so a session far down it
        // is not among `rows`. The store's own resolution has no cap; a
        // reference it cannot settle (no match, or ambiguous) is a miss.
        if let Ok(info) = host.find_session(query) {
            return Ok(summarize_row(&info, live_or_stored_count(&counts, &info)));
        }
        Err(ResolveMiss::NotFound {
            query: query.into(),
        })
    }

    fn resolve_model(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
        query: &str,
    ) -> Result<ModelRoute, ResolveMiss> {
        if !query.starts_with("dalgon/")
            && let Some(found) = crate::ext::synthetic::find(&self.host.shared, query)
        {
            return Ok(found.route());
        }
        let resolved = self.resolve_model_inner(query)?;
        if let Some(scoped) = &self.scoped {
            let id = format!("{}/{}", resolved.provider, model_name(&resolved.route));
            if !scoped.iter().any(|entry| entry.as_ref() == id) {
                return Err(ResolveMiss::NotFound {
                    query: query.into(),
                });
            }
        }
        Ok(resolved.route)
    }

    fn catalog(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
    ) -> Option<CatalogView> {
        self.host
            .shared
            .cached_catalog()
            .map(|catalog| CatalogView {
                entries: catalog.entries().to_vec(),
                providers: catalog
                    .providers()
                    .map(|provider| (provider.id.clone(), provider.family))
                    .collect(),
            })
    }

    fn levels_for(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
    ) -> Vec<ThinkingLevel> {
        let route = if let Some(route) = &self.route {
            route.clone()
        } else {
            let Some(default) = self.host.shared.config.model() else {
                return Vec::new();
            };
            match self.resolve_model_inner(default) {
                Ok(resolved) => resolved.route,
                Err(_) => return Vec::new(),
            }
        };
        if let Some(found) = crate::ext::synthetic::find_route(&self.host.shared, &route) {
            return dal_provider::levels_for(&found.entry().thinking).into_vec();
        }
        let Some(catalog) = self.host.shared.cached_catalog() else {
            return Vec::new();
        };
        let aliases: Vec<(Box<str>, Box<str>)> = Vec::new();
        let Ok(resolved) = dal_provider::resolve_route(&catalog, &aliases, &route) else {
            return Vec::new();
        };
        dal_provider::levels_for(&resolved.entry.thinking).into_vec()
    }

    fn auth_stored(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
    ) -> Vec<(Box<str>, Box<str>)> {
        let path = self.data_root.join("auth.json");
        let Ok(store) = dal_provider::AuthStore::load(path) else {
            return Vec::new();
        };
        store
            .status()
            .into_iter()
            .map(|row| (row.provider, row.kind.to_string().into()))
            .collect()
    }

    fn auth_remove(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
        provider: &str,
    ) -> bool {
        let path = self.data_root.join("auth.json");
        let Ok(mut store) = dal_provider::AuthStore::load(path) else {
            return false;
        };
        if !store.remove(provider) {
            return false;
        }
        store.store().is_ok()
    }

    fn docs_page(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
        uri: &str,
    ) -> Box<str> {
        let host = crate::host::Host {
            state: Arc::clone(&self.host),
        };
        host.doc(uri).map(|doc| doc.text).unwrap_or_default()
    }

    fn changelog_uri(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
    ) -> &'static str {
        "dal://changelog"
    }

    fn edit_style_for(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
        route: &ModelRoute,
    ) -> Box<str> {
        match self.host.shared.config.edit_style() {
            dal_core::EditStyleInput::Scalar(style) => style.clone(),
            dal_core::EditStyleInput::Table(rows) => {
                let reference = crate::host::ops::request_reference(route);
                for (pattern, style) in rows {
                    if pattern.as_ref() == "default" {
                        continue;
                    }
                    if style_match(pattern, &reference) {
                        return style.clone();
                    }
                }
                rows.iter()
                    .find(|(pattern, _)| pattern.as_ref() == "default")
                    .map(|(_, style)| style.clone())
                    .unwrap_or_default()
            }
        }
    }

    fn leaf_entries(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
    ) -> Vec<EntryView> {
        self.leaf.clone()
    }

    fn history_page(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
        query: PageReq,
    ) -> BoxFuture<'_, Result<Page<EntryView>, crate::error::ServiceError>> {
        Box::pin(async move { Ok(self.read_history_page(query).await) })
    }

    fn current_extensions(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
    ) -> Vec<crate::ext::Extension> {
        self.host.shared.generation.borrow().extensions.to_vec()
    }

    fn publish_generation(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<TurnId>,
        generation: crate::ext::generation::Generation,
    ) {
        let base: Vec<Box<str>> = generation
            .extensions
            .iter()
            .filter(|ext| ext.origin() != dal_core::Origin::User)
            .map(|ext| ext.name().into())
            .collect();
        debug_assert_eq!(base, self.host.shared.plugin_base);
        self.host
            .shared
            .generation
            .send_replace(Arc::new(generation));
    }
}

/// Counts leaf entries for live sessions by identity.
fn live_counts(host: &Arc<HostState>) -> HashMap<SessionId, u64> {
    let sessions = host
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    sessions
        .iter()
        .map(|(id, entry)| {
            let count = entry
                .shared
                .snapshot(crate::session::projection::SnapshotArgs {
                    generation: entry.generation,
                    id: *id,
                    workspace: entry.workspace.clone(),
                    open: Vec::new(),
                    updated_at: dal_core::Timestamp::now(),
                    created_at: None,
                    archived: None,
                    page: leaf_page(),
                })
                .entries
                .items
                .len() as u64;
            (*id, count)
        })
        .collect()
}

/// The leaf-path length a listing row reports: the live session's own count
/// when it is open here, else the count the store folded from its journal,
/// else zero for a row whose journal could not be read.
fn live_or_stored_count(counts: &HashMap<SessionId, u64>, info: &dal_core::SessionInfo) -> u64 {
    counts
        .get(&info.id)
        .copied()
        .or(info.message_count)
        .unwrap_or(0)
}

/// Maps one store row to its command summary.
fn summarize_row(info: &dal_core::SessionInfo, message_count: u64) -> SessionSummary {
    SessionSummary {
        id: info.id,
        name: info.name.clone(),
        message_count,
        updated_at: info.updated_at,
    }
}

/// Renders the model name of one route for scope checks.
fn model_name(route: &ModelRoute) -> String {
    match route {
        ModelRoute::Api { model, .. } => model.to_string(),
        ModelRoute::Synthetic { id } | ModelRoute::Harness { id } => id.to_string(),
    }
}

/// Matches one `*`-wildcard style pattern against a model reference.
fn style_match(pattern: &str, reference: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return true;
    };
    if !reference.starts_with(first) {
        return false;
    }
    let mut rest = &reference[first.len()..];
    for part in parts {
        if part.is_empty() {
            continue;
        }
        let Some(index) = rest.find(part) else {
            return false;
        };
        rest = &rest[index + part.len()..];
    }
    pattern.ends_with('*') || rest.is_empty()
}

impl DriverHost {
    pub(crate) async fn leaf_export_snapshot(&self) -> (View, Vec<EntryView>) {
        let (view, entries) = self.shared.snapshot_with_leaf_entries(SnapshotArgs {
            generation: self.generation,
            id: self.session,
            workspace: self.workspace.clone(),
            open: self.broker.open_requests(),
            updated_at: dal_core::Timestamp::now(),
            created_at: None,
            archived: None,
            page: PageReq::new(NonZeroU32::MIN, None).unwrap_or_default(),
        });
        (view, hydrate(&self.handle, &entries).await)
    }

    async fn read_history_page(&self, query: PageReq) -> Page<EntryView> {
        let view = self.shared.snapshot(SnapshotArgs {
            generation: self.generation,
            id: self.session,
            workspace: self.workspace.clone(),
            open: self.broker.open_requests(),
            updated_at: dal_core::Timestamp::now(),
            created_at: None,
            archived: None,
            page: query,
        });
        Page {
            items: hydrate(&self.handle, &view.entries.items).await,
            next_before: view.entries.next_before,
        }
    }

    /// Resolves one model reference through the cached catalog and aliases.
    fn resolve_model_inner(&self, query: &str) -> Result<dal_provider::ResolvedModel, ResolveMiss> {
        let Some(catalog) = self.host.shared.cached_catalog() else {
            return Err(ResolveMiss::NotFound {
                query: query.into(),
            });
        };
        let aliases: Vec<(Box<str>, Box<str>)> = self
            .host
            .shared
            .config
            .aliases()
            .iter()
            .map(|(name, target)| (name.clone(), target.clone()))
            .collect();
        dal_provider::resolve(&catalog, &aliases, query).map_err(|_| ResolveMiss::NotFound {
            query: query.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{export_log, job_log_outcome};
    use dal_core::JobOutcome;

    /// A job log whose parent directory cannot exist must surface an io
    /// error: `start_job` maps it to `JobOutcome::Failed`, not a silent
    /// `Exited { code: 0 }` with an empty log.
    #[tokio::test]
    async fn export_log_reports_unwritable_parent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocker = dir.path().join("blocker");
        tokio::fs::write(&blocker, b"x")
            .await
            .expect("write blocker file");
        let log_path = blocker.join("job.log");
        let error = export_log("{}", &log_path)
            .await
            .expect_err("a regular file cannot be a parent directory");
        assert!(
            !error.to_string().is_empty(),
            "the io error carries a description"
        );
    }

    /// A writable jobs dir receives the payload verbatim.
    #[tokio::test]
    async fn export_log_writes_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("nested").join("job.log");
        export_log("{\"ok\":true}", &log_path)
            .await
            .expect("export_log writes under a writable dir");
        let text = tokio::fs::read_to_string(&log_path)
            .await
            .expect("read written log");
        assert_eq!(text, "{\"ok\":true}");
    }

    /// The job-outcome boundary: an unwritten export log must become
    /// `JobOutcome::Failed`, never a silent `Exited { code: 0 }`.
    #[tokio::test]
    async fn job_log_outcome_maps_write_failure_to_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocker = dir.path().join("blocker");
        tokio::fs::write(&blocker, b"x")
            .await
            .expect("write blocker file");
        let outcome = job_log_outcome("export", "{}", &blocker.join("job.log")).await;
        let JobOutcome::Failed { message } = outcome else {
            panic!("a failed job-log write must not report a clean exit: {outcome:?}");
        };
        assert!(message.contains("job.log"), "the failure names the target");
        assert!(
            message.contains("export"),
            "the failure names the job label"
        );
    }

    /// The job-outcome boundary, happy path: a written log exits 0.
    #[tokio::test]
    async fn job_log_outcome_maps_written_log_to_exited() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("job.log");
        let outcome = job_log_outcome("export", "{}", &log_path).await;
        assert!(matches!(outcome, JobOutcome::Exited { code: 0 }));
    }

    /// The save-error boundary: post-apply persistence failures map to the
    /// typed `Failed` outcome with the message preserved; every other
    /// non-closed [`AgentError`] maps to `Rejected`; only a closed session
    /// keeps its own identity. Asserts on variants and message passthrough,
    /// never on product prose.
    #[test]
    fn save_error_separates_failed_rejected_and_closed() {
        use super::save_error;
        use crate::error::{ActualTurn, AgentError, ExpectedTurn, ValidationError};
        use crate::ext::command::SaveError;
        use dal_core::{BlobId, ClientId, RequestId, SessionId, TurnId};
        use std::num::NonZeroU64;

        let session = SessionId::new_v7();
        let rejections = [
            AgentError::WrongTurn {
                expected: ExpectedTurn::NoRunningTurn,
                actual: ActualTurn::Idle,
            },
            AgentError::AlreadyResolved {
                id: RequestId::new_v7(),
                by: ClientId::new("core"),
            },
            AgentError::SteerFull {
                turn: TurnId::new(NonZeroU64::new(1).expect("nonzero turn")),
            },
            AgentError::Invalid(ValidationError::new("bad answer")),
            AgentError::BlobNotFound {
                id: BlobId::from_bytes(b"missing"),
                session,
            },
            AgentError::SessionGone { session },
        ];
        for error in rejections {
            assert!(
                matches!(save_error(error), SaveError::Rejected { .. }),
                "a rejection without state change must not report a save failure"
            );
        }
        assert!(matches!(
            save_error(AgentError::SessionClosed { id: session }),
            SaveError::SessionClosed { .. }
        ));
        let text = "journal write failed: disk full";
        assert!(matches!(
            save_error(AgentError::PersistenceFailed {
                message: text.into()
            }),
            SaveError::Failed { message } if message.as_ref() == text
        ));
    }

    #[test]
    fn session_and_default_intent_reaches_the_default_writer() {
        use dal_core::{ApprovalMode, Command, Family, Mode, ModelRoute, Save, ThinkingLevel};

        let route = || ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-4o".into(),
            provider: Some("beta".into()),
        };
        assert!(
            super::persist_for(&Command::SetModel {
                model: route(),
                save: Save::SessionAndDefault,
            })
            .is_some()
        );
        assert!(
            super::persist_for(&Command::SetModel {
                model: route(),
                save: Save::SessionOnly,
            })
            .is_none()
        );
        assert!(
            super::persist_for(&Command::SetThinking {
                level: ThinkingLevel::High,
                save: Save::SessionAndDefault,
            })
            .is_some()
        );
        assert!(
            super::persist_for(&Command::SetThinking {
                level: ThinkingLevel::High,
                save: Save::SessionOnly,
            })
            .is_none()
        );
        assert!(
            super::persist_for(&Command::SetApproval {
                mode: ApprovalMode::All,
                save: Save::SessionAndDefault,
            })
            .is_some()
        );
        assert!(
            super::persist_for(&Command::SetApproval {
                mode: ApprovalMode::All,
                save: Save::SessionOnly,
            })
            .is_none()
        );
        assert!(
            super::persist_for(&Command::SetMode {
                mode: Mode::EvalFirst,
                save: Save::SessionAndDefault,
            })
            .is_some()
        );
        assert!(
            super::persist_for(&Command::SetMode {
                mode: Mode::EvalFirst,
                save: Save::SessionOnly,
            })
            .is_none()
        );
        assert!(super::persist_for(&Command::SetScopedModels(vec!["a/b".into()])).is_none());
    }

    #[test]
    fn default_model_text_names_the_provider_never_the_family() {
        use dal_core::{Family, ModelRoute};

        let provider = ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-4o".into(),
            provider: Some("beta".into()),
        };
        assert_eq!(super::default_model_text(&provider), "beta/gpt-4o");
        let legacy = ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-4o".into(),
            provider: None,
        };
        assert_eq!(
            super::default_model_text(&legacy),
            legacy.family_qualified_reference()
        );
        let synthetic = ModelRoute::Synthetic {
            id: "fixture/end".into(),
        };
        assert_eq!(super::default_model_text(&synthetic), "fixture/end");
    }

    #[tokio::test]
    async fn session_and_default_model_writes_the_provider_qualified_key() {
        use dal_core::{Config, ConfigProduct, Family, ModelRoute};
        use std::collections::BTreeMap;

        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).expect("data dir");
        let config_path = tmp.path().join("dal.toml");
        let initial = "# kept user comment\nmodel = \"alpha/gpt-4o\"\n";
        std::fs::write(&config_path, initial).expect("seed config");
        let config =
            Config::load(ConfigProduct::Dalgon, &data, "", Some(initial)).expect("config loads");
        let product = crate::host::Product {
            name: "dal",
            data_root: data.clone(),
            defaults: "",
            extensions: Vec::new(),
            bundled: Vec::new(),
        };
        let env = crate::host::Env {
            vars: BTreeMap::new(),
            cwd: tmp.path().to_path_buf(),
            sandbox_helper: None,
        };
        let host = crate::host::Host::start(product, config, env)
            .await
            .expect("host");
        host.set_config_path(config_path.clone());
        let route = ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-4o".into(),
            provider: Some("beta".into()),
        };
        super::persist_default(&host.state, super::PersistDefault::Model(route))
            .await
            .expect("default persists");
        let saved = std::fs::read_to_string(&config_path).expect("config file reads");
        assert!(saved.contains("# kept user comment"), "{saved}");
        assert!(saved.contains("model = \"beta/gpt-4o\""), "{saved}");
        assert!(!saved.contains("model_route"), "{saved}");
        assert!(!saved.contains("openai-chat/gpt-4o"), "{saved}");
        let reloaded = Config::load(ConfigProduct::Dalgon, &data, "", Some(saved.as_str()))
            .expect("saved file loads");
        assert_eq!(reloaded.model(), Some("beta/gpt-4o"));
        let _ = host.shutdown(std::time::Duration::from_secs(5)).await;
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one end-to-end persistence proof through the real host"
    )]
    #[tokio::test]
    async fn command_host_submit_wait_persists_session_and_default_model() {
        use crate::ext::command::CommandCx;
        use crate::ext::{Caller, CallerKind};
        use crate::session::driver::DriverDeps;
        use dal_core::{
            ClientId, Command, Config, ConfigProduct, Family, ModelRoute, Name, Origin, Save,
            ServiceSet, Workspace,
        };
        use std::collections::BTreeMap;
        use std::num::NonZeroU32;
        use std::sync::Arc;

        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        let workspace_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let config_path = tmp.path().join("dal.toml");
        let initial = "# kept user comment\nmodel = \"alpha/gpt-4o\"\n";
        std::fs::write(&config_path, initial).expect("seed config");
        let config =
            Config::load(ConfigProduct::Dalgon, &data, "", Some(initial)).expect("config loads");
        let product = crate::host::Product {
            name: "dal",
            data_root: data.clone(),
            defaults: "",
            extensions: Vec::new(),
            bundled: Vec::new(),
        };
        let env = crate::host::Env {
            vars: BTreeMap::new(),
            cwd: workspace_dir.clone(),
            sandbox_helper: None,
        };
        let host = crate::host::Host::start(product, config, env)
            .await
            .expect("host");
        host.set_config_path(config_path.clone());
        let workspace = Workspace::new(workspace_dir).expect("workspace");
        let agent = host
            .open(
                crate::SessionRef::New {
                    workspace: workspace.clone(),
                    name: None,
                },
                ClientId::new("default-persist"),
            )
            .await
            .expect("open");
        let id = agent.session();
        let deps = {
            let sessions = host
                .state
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = sessions.get(&id).expect("session entry");
            let services: Arc<dyn crate::ext::Services> = entry.services.clone();
            DriverDeps {
                session: id,
                parent: None,
                workspace: entry.workspace.clone(),
                host: Arc::clone(&host.state),
                backend: entry.backend.clone(),
                services,
                broker: entry.broker.clone(),
                jobs: Arc::new(tokio::sync::Mutex::new(crate::jobs::JobTable::new())),
                shared: entry.shared.clone(),
                overlay: entry.overlay.clone(),
                generation: entry.generation,
                handle: entry.handle.clone(),
                tasks: entry.tasks.clone(),
                cancel: tokio_util::sync::CancellationToken::new(),
                ephemeral: false,
            }
        };
        let driver = super::driver_host(&deps, None, None).await;
        let caller = Caller::new(
            Name::parse("test").expect("literal name parses"),
            Origin::Builtin,
            ServiceSet::EMPTY,
            NonZeroU32::MIN,
            CallerKind::Handler,
            None,
        );
        let cx = CommandCx::new(caller, id, None, deps.services.clone(), Arc::new(driver));
        let route = ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-4o".into(),
            provider: Some("beta".into()),
        };
        cx.submit_wait(Command::SetModel {
            model: route,
            save: Save::SessionAndDefault,
        })
        .await
        .expect("default persists");
        let saved = std::fs::read_to_string(&config_path).expect("config file reads");
        assert!(saved.contains("# kept user comment"), "{saved}");
        assert!(saved.contains("model = \"beta/gpt-4o\""), "{saved}");
        assert!(!saved.contains("model_route"), "{saved}");
        assert!(!saved.contains("openai-chat/gpt-4o"), "{saved}");
        let reloaded = Config::load(ConfigProduct::Dalgon, &data, "", Some(saved.as_str()))
            .expect("saved file loads");
        assert_eq!(reloaded.model(), Some("beta/gpt-4o"));
        let other = ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-4o".into(),
            provider: Some("alpha".into()),
        };
        cx.submit_wait(Command::SetModel {
            model: other,
            save: Save::SessionOnly,
        })
        .await
        .expect("session only applies");
        let again = std::fs::read_to_string(&config_path).expect("config file rereads");
        assert_eq!(
            again, saved,
            "a session-only save must leave the file bytes unchanged"
        );
        let _ = host.shutdown(std::time::Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn default_write_without_a_config_path_reports_failure() {
        use dal_core::{Config, ConfigProduct, Family, ModelRoute};
        use std::collections::BTreeMap;

        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).expect("data dir");
        let config = Config::load(
            ConfigProduct::Dalgon,
            &data,
            "",
            Some("model = \"alpha/gpt-4o\"\n"),
        )
        .expect("config loads");
        let product = crate::host::Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions: Vec::new(),
            bundled: Vec::new(),
        };
        let env = crate::host::Env {
            vars: BTreeMap::new(),
            cwd: tmp.path().to_path_buf(),
            sandbox_helper: None,
        };
        let host = crate::host::Host::start(product, config, env)
            .await
            .expect("host");
        let route = ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-4o".into(),
            provider: Some("beta".into()),
        };
        let error = super::persist_default(&host.state, super::PersistDefault::Model(route))
            .await
            .expect_err("no target must fail");
        assert!(error.to_string().contains("no config path"), "{error}");
        let _ = host.shutdown(std::time::Duration::from_secs(5)).await;
    }
}
