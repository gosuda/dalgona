//! Session table: open, close, shutdown, listing, and host updates.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dal_core::{
    ApprovalMode, ClientId, Effect, Gen, ListQuery, Name, Page, PageReq, Record, Session,
    SessionEnd, SessionId, SessionInfo, SessionStart, Timestamp, UpdateKind, Workspace,
};
use dal_store::{Journal, Store};
use serde::Deserialize;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::{
    Host, HostSubscription, HostUpdate, NameClaim, SessionEntry, SessionRef, ShutdownReport,
};
use crate::agent::{Agent, AgentInner};
use crate::broker::Broker;
use crate::error::HostError;
use crate::ext::generation::Generation;
use crate::ext::grants::GrantStore;
use crate::ext::hooks::{
    HookScope, ObserverReport, dispatch_session_end, dispatch_session_start, hook_fanout,
};
use crate::ext::script::ScriptCx;
use crate::ext::services::{SessionServices, SessionServicesDeps};
use crate::ext::{Caller, Services};
use crate::session::SessionHandle;
use crate::session::actor::{ActorDeps, DriverPorts, spawn};
use crate::session::backend::{Backend, BackendDeps};
use crate::session::rt::{SessionRt, SessionRtDeps};
use crate::session::script::SessionScriptHost;

/// Publishes one session-scoped notice per failed observer hook.
///
/// Session boundaries have no turn, so the notice carries no turn id; the
/// text already names the extension, event, and failure.
fn notify_observer_failure(
    services: &Arc<dyn Services>,
    caller: &Caller,
    event: &str,
    failed_before: u64,
    report: &ObserverReport,
) {
    if report.failed == failed_before {
        return;
    }
    let Some(text) = report.last_error.clone() else {
        return;
    };
    services.notify(
        caller,
        dal_core::Notice {
            turn: None,
            kind: format!("hook.{event}").into(),
            text,
        },
    );
}
use crate::session::shared::Shared;
use crate::session::tasks::SessionTasks;

/// How long one service `ask` question may stay open.
const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Sweep interval of the shutdown quiet wait; polls are bounded reads.
const STATUS_QUIET_POLL: Duration = Duration::from_millis(50);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildPolicyBody {
    tools: Option<Vec<Box<str>>>,
    approval: ApprovalMode,
}

struct ChildPolicy {
    tools: Option<Vec<Name>>,
    approval: ApprovalMode,
}

fn parse_child_policy_tool(name: &str) -> Result<Name, HostError> {
    Name::parse_mapped_tool(name).map_err(|error| HostError::Config {
        message: format!("invalid child policy tool {name:?}: {error}").into(),
    })
}

/// Reads the newest host-owned child-policy record a journal carries; a
/// journal without one leaves the session unrestricted and on the configured
/// mode. Provenance is enforced when records are constructed; the journal
/// format does not carry a second authentication field.
fn replay_child_policy(records: &[Record]) -> Result<Option<ChildPolicy>, HostError> {
    let found = records.iter().rev().find(|record| {
        matches!(
            record,
            Record::Ext { ext, kind, .. }
                if super::is_child_policy_record(ext.as_ref(), kind.as_ref())
        )
    });
    let Some(Record::Ext { body, .. }) = found else {
        return Ok(None);
    };
    let body: ChildPolicyBody =
        sonic_rs::from_str(body.as_str()).map_err(|error| HostError::Config {
            message: format!("invalid child policy record: {error}").into(),
        })?;
    let tools = body
        .tools
        .map(|names| {
            names
                .into_iter()
                .map(|name| parse_child_policy_tool(&name))
                .collect()
        })
        .transpose()?;
    Ok(Some(ChildPolicy {
        tools,
        approval: body.approval,
    }))
}

/// The replayed state a spawned session starts from.
struct ReplayedSession {
    /// The session fold built from journal replay.
    fold: Session,
    /// The journal generation.
    generation: Gen,
    /// The session data plane.
    shared: Arc<Shared>,
    /// The entry snapshot schemes expose at start.
    initial_entries: Arc<[dal_core::EntryView]>,
    /// The session-start conversation texts.
    opening_texts: Arc<[String]>,
    /// Effects replay queued for the actor.
    pending: Vec<Effect>,
    /// The session job table.
    jobs: Arc<tokio::sync::Mutex<crate::jobs::JobTable>>,
}

/// The wired runtime pieces of one spawned session.
struct WiredSession {
    /// The actor handle.
    handle: SessionHandle,
    /// The actor's driver ports.
    ports: DriverPorts,
    /// The actor task.
    task: tokio::task::JoinHandle<()>,
    /// The session backend.
    backend: Arc<Backend>,
    /// The session services.
    services: Arc<SessionServices>,
    /// The turn overlay.
    overlay: Arc<crate::ext::overlay::Overlay>,
    /// The extension generation live at wire time.
    ext_generation: Arc<Generation>,
    /// The journal generation.
    generation: Gen,
    /// The request broker.
    broker: Arc<Broker>,
    /// The session data plane.
    shared: Arc<Shared>,
    /// The session job table.
    jobs: Arc<tokio::sync::Mutex<crate::jobs::JobTable>>,
}

impl Host {
    /// Resolves a session reference, replays its journal, and spawns its actor.
    ///
    /// A second open of a live session returns the same actor bound to the
    /// new client. Workspaces always come from the reference, never the
    /// process working directory.
    ///
    /// # Errors
    ///
    /// Returns an error when resolving, replaying, or spawning the session fails.
    pub async fn open(&self, session: SessionRef, by: ClientId) -> Result<Agent, HostError> {
        if let SessionRef::Resume { key, .. } = &session
            && let Ok(id) = SessionId::parse(key)
            && let Some(entry) = self.entry_of(id)
        {
            return Ok(Self::bind(&entry, id, by));
        }
        let resolved = self.resolve_ref(&session)?;
        if let Some(entry) = self.entry_of(resolved.id) {
            return Ok(Self::bind(&entry, resolved.id, by));
        }
        let name_claim = self.claim_name(&resolved)?;
        self.spawn_session(resolved, by, None, name_claim).await
    }

    /// Flushes one session, stops its actor, and removes it from the table.
    ///
    /// # Errors
    ///
    /// Returns an error when `id` is not open or its actor cannot be shut down.
    pub async fn close(&self, id: SessionId) -> Result<(), HostError> {
        let entry = self.take_entry(id)?;
        let generation = self.state.shared.generation.borrow().clone();
        let event = SessionEnd {
            session: id,
            reason: "close".into(),
        };
        let services = Arc::clone(&entry.services);
        let cancel = entry.cancel.clone();
        let tasks = entry.tasks.clone();
        let entry_parent = entry.parent;
        let script = SessionScriptHost::for_generation(
            id,
            &entry.backend,
            Arc::clone(&self.state.shared.interpreters),
            Arc::clone(&generation),
        )
        .attach(None);
        let process_env = Arc::clone(&self.state.shared.env);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        tasks.spawn(async move {
            observe_session_end(
                &generation,
                &services,
                &cancel,
                entry_parent,
                &process_env,
                script,
                &event,
            )
            .await;
            let _ = done_tx.send(());
        });
        let _ = done_rx.await;
        entry.cancel.cancel();
        entry.driver.abort();
        entry.handle.shutdown().await;
        entry.overlay.clear();
        let _ = entry.task.await;
        self.publish(&HostUpdate::SessionRemoved { session: id });
        Ok(())
    }

    /// Closes every session within the grace period and reports the count.
    pub async fn shutdown(self, grace: Duration) -> ShutdownReport {
        self.shutdown_with_status_wait(Some(grace), grace).await
    }

    /// Closes every session after the caller has completed the quiet wait.
    ///
    /// Use this when a front end already waited for status kinds and must not
    /// spend the same status grace a second time.
    pub async fn shutdown_after_quiet_wait(self, close_grace: Duration) -> ShutdownReport {
        self.shutdown_with_status_wait(None, close_grace).await
    }

    async fn shutdown_with_status_wait(
        self,
        status_grace: Option<Duration>,
        close_grace: Duration,
    ) -> ShutdownReport {
        let status_quiet = match status_grace {
            Some(grace) => self.await_status_quiet(grace).await,
            None => false,
        };
        self.stop_attached().await;
        let ids: Vec<SessionId> = self.table_ids();
        let task_owners: Vec<_> = self
            .state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|entry| entry.tasks.clone())
            .collect();
        let mut sessions_closed = 0;
        for id in ids {
            let this = self.clone();
            let closed = timeout(close_grace, this.close(id))
                .await
                .is_ok_and(|outcome| outcome.is_ok());
            if closed {
                sessions_closed += 1;
            }
        }
        let tasks_remaining = task_owners.iter().fold(0_usize, |total, tasks| {
            total.saturating_add(tasks.tracked())
        });
        ShutdownReport {
            sessions_closed,
            status_quiet,
            tasks_remaining,
        }
    }

    /// Aborts every `Attach` controller and waits until each has ended.
    async fn stop_attached(&self) {
        let mut tasks = std::mem::take(
            &mut *self
                .state
                .attached
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }

    /// Waits until every open session's registered status kinds report quiet,
    /// polling each actor before accepting the quiet predicate.
    async fn await_status_quiet(&self, grace: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            match tokio::time::timeout_at(deadline, self.statuses_quiet()).await {
                Ok(true) => return true,
                Ok(false) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep_until(
                        (tokio::time::Instant::now() + STATUS_QUIET_POLL).min(deadline),
                    )
                    .await;
                }
                Ok(false) | Err(_) => return false,
            }
        }
    }

    /// Polls every open session and reports whether all its status kinds are
    /// quiet. Sessions that close during this query are excluded on retry.
    pub async fn is_quiet(&self) -> bool {
        self.statuses_quiet().await
    }

    async fn statuses_quiet(&self) -> bool {
        let handles: Vec<_> = self
            .state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|entry| entry.handle.clone())
            .collect();
        for handle in handles {
            let Ok(statuses) = handle.poll_status().await else {
                return false;
            };
            if statuses.iter().any(|status| !status.is_quiet()) {
                return false;
            }
        }
        true
    }

    /// Pages session rows across the host's known workspaces.
    ///
    /// # Errors
    ///
    /// Returns an error if listing sessions from a workspace fails.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the public API owns its query while paging across workspaces"
    )]
    pub fn sessions(&self, query: ListQuery) -> Result<Page<SessionInfo, Box<str>>, HostError> {
        let mut rows: HashMap<SessionId, SessionInfo> = HashMap::new();
        for workspace in self.workspaces() {
            let store = self.store_for(&workspace);
            let page = store.list(query.clone())?;
            rows.extend(page.items.into_iter().map(|info| (info.id, info)));
        }
        for (id, entry) in self.entries() {
            rows.entry(id)
                .or_insert_with(|| entry.shared.snapshot(snapshot_args(&entry)).session);
        }
        let limit = query.limit.unwrap_or(u32::MAX);
        let mut items: Vec<SessionInfo> = rows.into_values().collect();
        items.sort_by_key(|info| std::cmp::Reverse(info.updated_at));
        items.truncate(limit as usize);
        Ok(Page {
            items,
            next_before: None,
        })
    }

    /// Registers a host lifecycle subscription.
    #[must_use]
    pub fn subscribe(&self) -> HostSubscription {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.state
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(tx);
        HostSubscription { receiver: rx }
    }

    fn entry_of(&self, id: SessionId) -> Option<SessionPorts> {
        let sessions = self
            .state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.get(&id).map(|entry| SessionPorts {
            id,
            handle: entry.handle.clone(),
            shared: Arc::clone(&entry.shared),
            broker: Arc::clone(&entry.broker),
            workspace: entry.workspace.clone(),
            generation: entry.generation,
            control: Arc::clone(&entry.control),
        })
    }

    fn bind(ports: &SessionPorts, id: SessionId, by: ClientId) -> Agent {
        Agent {
            inner: Arc::new(AgentInner {
                session: id,
                client: by,
                handle: ports.handle.clone(),
                shared: Arc::clone(&ports.shared),
                broker: Arc::clone(&ports.broker),
                workspace: ports.workspace.clone(),
                generation: ports.generation,
                control: Arc::clone(&ports.control),
                created_at: None,
                archived: None,
            }),
        }
    }
}

/// The shareable ports of one live session.
struct SessionPorts {
    id: SessionId,
    handle: crate::session::SessionHandle,
    shared: Arc<Shared>,
    broker: Arc<Broker>,
    workspace: Workspace,
    generation: dal_core::Gen,
    control: Arc<std::sync::Mutex<crate::session::control::ControlCell>>,
}

/// A resolved session reference awaiting spawn.
struct ResolvedRef {
    id: SessionId,
    workspace: Workspace,
    depth: u32,
    ephemeral: bool,
    resumed: bool,
    child: bool,
    parent: Option<SessionId>,
    name: Option<Box<str>>,
}

impl Host {
    fn resolve_ref(&self, session: &SessionRef) -> Result<ResolvedRef, HostError> {
        match session {
            SessionRef::New { workspace, name } => Ok(ResolvedRef {
                id: SessionId::new_v7(),
                workspace: workspace.clone(),
                depth: 0,
                ephemeral: false,
                resumed: false,
                child: false,
                parent: None,
                name: name.clone(),
            }),
            SessionRef::Ephemeral { workspace } => Ok(ResolvedRef {
                id: SessionId::new_v7(),
                workspace: workspace.clone(),
                depth: 0,
                ephemeral: true,
                resumed: false,
                child: false,
                parent: None,
                name: None,
            }),
            SessionRef::Resume { key, workspace } => {
                let store = self.store_for(workspace);
                let id = store.resolve(workspace, key)?;
                Ok(ResolvedRef {
                    id,
                    workspace: workspace.clone(),
                    depth: 0,
                    ephemeral: false,
                    resumed: true,
                    child: false,
                    parent: None,
                    name: None,
                })
            }
            SessionRef::Continue { workspace } => {
                let store = self.store_for(workspace);
                let (id, resumed) = match store.newest()? {
                    Some(id) => (id, true),
                    None => (SessionId::new_v7(), false),
                };
                Ok(ResolvedRef {
                    id,
                    workspace: workspace.clone(),
                    depth: 0,
                    ephemeral: false,
                    resumed,
                    child: false,
                    parent: None,
                    name: None,
                })
            }
            SessionRef::Child {
                parent,
                workspace,
                name,
                ..
            } => {
                let depth = self.depth_of(*parent)? + 1;
                if depth > self.max_depth() {
                    return Err(HostError::max_depth(self.max_depth()));
                }
                Ok(ResolvedRef {
                    id: SessionId::new_v7(),
                    workspace: workspace.clone(),
                    depth,
                    ephemeral: false,
                    resumed: false,
                    child: true,
                    parent: Some(*parent),
                    name: name.clone(),
                })
            }
        }
    }

    fn claim_name(&self, resolved: &ResolvedRef) -> Result<Option<NameClaim>, HostError> {
        let Some(name) = resolved.name.as_ref() else {
            return Ok(None);
        };
        let key = (resolved.workspace.clone(), name.clone());
        let mut claims = self
            .state
            .name_claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let name_taken = |id| {
            dal_store::StoreError::NameTaken {
                name: name.clone(),
                id,
            }
            .into()
        };
        if let Some(id) = claims.get(&key).copied() {
            return Err(name_taken(id));
        }
        {
            let sessions = self
                .state
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let live = sessions.iter().find_map(|(id, entry)| {
                if entry.workspace != resolved.workspace {
                    return None;
                }
                let view = entry
                    .shared
                    .snapshot(crate::session::projection::SnapshotArgs {
                        generation: entry.generation,
                        id: *id,
                        workspace: entry.workspace.clone(),
                        open: Vec::new(),
                        updated_at: Timestamp::now(),
                        created_at: None,
                        archived: None,
                        page: PageReq::default(),
                    });
                (view.session.name.as_deref() == Some(name)).then_some(*id)
            });
            if let Some(id) = live {
                return Err(name_taken(id));
            }
        }
        claims.insert(key.clone(), resolved.id);
        Ok(Some(NameClaim {
            state: Arc::clone(&self.state),
            key,
            id: resolved.id,
        }))
    }
    /// Opens or creates the journal for `resolved` and applies its name.
    async fn open_session_journal(
        &self,
        resolved: &ResolvedRef,
        journal: Option<Journal>,
    ) -> Result<(Journal, bool), HostError> {
        let store = self.store_for(&resolved.workspace);
        let id = resolved.id;
        let (mut journal, resumed) = match journal {
            Some(journal) => (journal, false),
            None if resolved.ephemeral => (store.ephemeral_session(id), false),
            None => match store.open_session(id).await {
                Ok((journal, _)) => (journal, resolved.resumed),
                Err(dal_store::StoreError::NotFound { .. }) => (store.create_session(id), false),
                Err(error) => return Err(error.into()),
            },
        };
        if let Some(name) = resolved.name.as_deref() {
            // Buffered, not written: a lazy journal holds the name in its
            // in-memory record prefix, so a start cancelled before its first
            // user entry leaves no named session behind.
            journal.set_name(Some(name)).await?;
        }
        Ok((journal, resumed))
    }

    /// Replays the journal into the fold and shared data plane.
    async fn replay_session(
        &self,
        resolved: &ResolvedRef,
        journal: &mut Journal,
    ) -> Result<ReplayedSession, HostError> {
        let generation = journal.generation();
        let thinking = self.state.shared.config.thinking();
        let policy = replay_child_policy(journal.records())?;
        let approval = policy.as_ref().map_or_else(
            || self.state.shared.config.approval(),
            |policy| policy.approval,
        );
        let mode = self.state.shared.config.mode();
        let (fold, effects) = Session::replay_with(
            dal_core::Settings {
                model: self
                    .state
                    .shared
                    .config
                    .model()
                    .map(dal_core::ModelRoute::from_id),
                thinking,
                approval,
                name: None,
                mode,
            },
            journal.records().to_vec(),
            Timestamp::now(),
        )
        .map_err(|error| HostError::Io {
            path: self.state.shared.data_root.clone(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()),
        })?;
        let opening_texts: Arc<[String]> = super::history::opening_texts(&fold).into();
        let shared = Arc::new(Shared::new(
            resolved.child,
            generation,
            thinking,
            approval,
            mode,
        ));
        shared.restore_fold(&fold);
        if let Some(policy) = &policy
            && let Some(tools) = &policy.tools
        {
            shared.restrict_tools(tools);
        }
        let initial_entries: Arc<[dal_core::EntryView]> = shared.leaf_entries().into();
        let jobs_table = if resolved.ephemeral {
            crate::jobs::JobTable::new()
        } else {
            let jobs_dir = crate::session::commands::host::session_jobs_dir(
                &self.state,
                &resolved.workspace,
                resolved.id,
            );
            crate::jobs::JobTable::open(jobs_dir, fold.delivered_jobs())
                .await
                .map_err(|error| HostError::Io {
                    path: self.state.shared.data_root.clone(),
                    source: std::io::Error::other(error.to_string()),
                })?
        };
        let jobs = Arc::new(tokio::sync::Mutex::new(jobs_table));
        let mut pending = Vec::new();
        self.execute_replay(journal, &shared, effects, &mut pending)
            .await?;
        Ok(ReplayedSession {
            fold,
            generation,
            shared,
            initial_entries,
            opening_texts,
            pending,
            jobs,
        })
    }

    /// Spawns the actor, backend, runtime, and services for one session.
    async fn wire_session(
        &self,
        resolved: &ResolvedRef,
        journal: Journal,
        replayed: ReplayedSession,
        cancel: &CancellationToken,
        tasks: &SessionTasks,
    ) -> Result<WiredSession, HostError> {
        let ReplayedSession {
            fold,
            generation,
            shared,
            initial_entries,
            opening_texts,
            pending,
            jobs,
        } = replayed;
        let broker = Arc::new(Broker::new());
        // The actor receives the data-plane through a cell: `Backend::new`
        // needs the actor handle `spawn` returns, and hook chains mint their
        // script hosts only once a request arrives, after the fill below.
        let backend_cell = Arc::new(std::sync::OnceLock::new());
        let (handle, ports, task) = spawn(ActorDeps {
            session: resolved.id,
            journal,
            fold,
            shared: Arc::clone(&shared),
            broker: Arc::clone(&broker),
            workspace: resolved.workspace.clone(),
            depth: resolved.depth,
            parent: resolved.parent,
            pending,
            host: Arc::clone(&self.state),
            tasks: tasks.clone(),
            backend: Arc::clone(&backend_cell),
        });
        let backend = Arc::new(Backend::new(BackendDeps {
            session: resolved.id,
            workspace: resolved.workspace.clone(),
            host: Arc::clone(&self.state),
            shared: Arc::clone(&shared),
            initial_entries: Arc::clone(&initial_entries),
            broker: Arc::clone(&broker),
            handle: handle.clone(),
            jobs: Arc::clone(&jobs),
            cancel: cancel.clone(),
            tasks: tasks.clone(),
        }));
        let _ = backend_cell.set(Arc::clone(&backend));
        let rt = Arc::new(SessionRt::new(SessionRtDeps {
            workspace: resolved.workspace.clone(),
            shared: Arc::clone(&shared),
            initial_entries: Arc::clone(&initial_entries),
            scheme_store: Arc::clone(backend.scheme_store()),
            host: Arc::clone(&self.state),
            jobs: Arc::clone(&jobs),
            procs: Arc::clone(backend.procs()),
            env_snapshot: backend.env_snapshot().to_vec(),
            launcher: backend.launcher().clone(),
            approval: self.state.shared.config.approval(),
            cancel: cancel.clone(),
            jobs_dir: backend.jobs_dir().to_path_buf(),
            tasks: tasks.clone(),
        }));
        let grants = Arc::new(
            GrantStore::with_runtime(
                self.state.shared.data_root.clone(),
                ASK_TIMEOUT,
                Arc::clone(&broker),
            )
            .map_err(|error| HostError::Config {
                message: error.to_string().into(),
            })?,
        );
        let ext_generation = self.state.shared.generation.borrow().clone();
        let overlay = Arc::new(crate::ext::overlay::Overlay::with_grants(
            Arc::clone(&grants),
            cancel.clone(),
        ));
        let services = Arc::new(SessionServices::new(SessionServicesDeps {
            grants,
            broker: Arc::clone(&broker),
            backend: backend.clone(),
            rt,
            mcp_client: ext_generation.mcp().map(|(_, client)| Arc::clone(client)),
            generation: self.state.shared.generation.subscribe(),
            overlay: Arc::clone(&overlay),
            history: opening_texts,
            sites: HashMap::new(),
            cancel: cancel.clone(),
            ask_timeout: ASK_TIMEOUT,
            ephemeral: resolved.ephemeral,
            workspace: resolved.workspace.clone(),
        }));
        backend.set_services(&services);
        // The actor task was just spawned, so its command channel is open;
        // a closed channel here means the actor already failed, and the
        // first client operation reports it as `session.closed`.
        let _ = handle.services(services.clone()).await;
        Ok(WiredSession {
            handle,
            ports,
            task,
            backend,
            services,
            overlay,
            ext_generation,
            generation,
            broker,
            shared,
            jobs,
        })
    }

    /// Fires the `session_start` observer for the wired session.
    async fn observe_start(
        &self,
        resolved: &ResolvedRef,
        wired: &WiredSession,
        cancel: &CancellationToken,
        resumed: bool,
    ) {
        let start_event = SessionStart {
            session: resolved.id,
            workspace: resolved.workspace.clone(),
            resumed,
        };
        let observer_services: Arc<dyn Services> = wired.services.clone() as Arc<dyn Services>;
        let observer_script = SessionScriptHost::for_generation(
            resolved.id,
            &wired.backend,
            Arc::clone(&self.state.shared.interpreters),
            Arc::clone(&wired.ext_generation),
        )
        .attach(None);
        // Delivered before the Agent is bound: a `before_turn` hook on the
        // first prompt must already see the state a `session_start`
        // observer just inserted.
        observe_session_start(
            &wired.ext_generation,
            &observer_services,
            cancel,
            resolved.parent,
            &self.state.shared.env,
            observer_script,
            &start_event,
        )
        .await;
    }

    /// Spawns the driver, registers the session entry, and publishes the
    /// host update; returns the bound agent.
    fn activate_session(
        &self,
        resolved: &ResolvedRef,
        wired: WiredSession,
        cancel: CancellationToken,
        tasks: &SessionTasks,
        by: ClientId,
        name_claim: Option<NameClaim>,
    ) -> Agent {
        let id = resolved.id;
        let control = wired.ports.control.clone();
        let driver = crate::session::driver::spawn(
            wired.ports,
            crate::session::driver::DriverDeps {
                session: id,
                parent: resolved.parent,
                workspace: resolved.workspace.clone(),
                host: Arc::clone(&self.state),
                backend: Arc::clone(&wired.backend),
                services: Arc::clone(&wired.services) as Arc<dyn crate::ext::Services>,
                broker: Arc::clone(&wired.broker),
                jobs: Arc::clone(&wired.jobs),
                shared: Arc::clone(&wired.shared),
                overlay: Arc::clone(&wired.overlay),
                generation: wired.generation,
                handle: wired.handle.clone(),
                tasks: tasks.clone(),
                cancel: cancel.clone(),
                ephemeral: resolved.ephemeral,
            },
        );
        let ports = SessionPorts {
            id,
            handle: wired.handle,
            shared: wired.shared,
            broker: wired.broker,
            workspace: resolved.workspace.clone(),
            generation: wired.generation,
            control: Arc::clone(&control),
        };
        let agent = Self::bind(&ports, id, by);
        self.state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id,
                SessionEntry {
                    handle: ports.handle,
                    shared: ports.shared,
                    broker: ports.broker,
                    services: wired.services.clone(),
                    tasks: tasks.clone(),
                    _name_claim: name_claim,
                    workspace: ports.workspace,
                    depth: resolved.depth,
                    parent: resolved.parent,
                    generation: ports.generation,
                    driver,
                    task: wired.task,
                    control: ports.control,
                    cancel,
                    backend: Arc::clone(&wired.backend),
                    overlay: wired.overlay,
                    reported: std::sync::atomic::AtomicBool::new(false),
                    prompted: std::sync::atomic::AtomicBool::new(false),
                },
            );
        if let Some(parent) = resolved.parent {
            self.publish(&HostUpdate::ChildStarted {
                parent,
                child: id,
                name: String::new().into(),
            });
        } else {
            self.publish(&HostUpdate::SessionAdded { session: id });
        }
        agent
    }

    async fn spawn_session(
        &self,
        resolved: ResolvedRef,
        by: ClientId,
        journal: Option<Journal>,
        name_claim: Option<NameClaim>,
    ) -> Result<Agent, HostError> {
        let (mut journal, resumed) = self.open_session_journal(&resolved, journal).await?;
        let replayed = self.replay_session(&resolved, &mut journal).await?;
        let cancel = CancellationToken::new();
        let tasks = SessionTasks::new();
        let wired = self
            .wire_session(&resolved, journal, replayed, &cancel, &tasks)
            .await?;
        self.observe_start(&resolved, &wired, &cancel, resumed)
            .await;
        Ok(self.activate_session(&resolved, wired, cancel, &tasks, by, name_claim))
    }

    /// Launches a pre-branched journal as a child session of `parent`.
    pub(crate) async fn launch_branched(
        &self,
        journal: Journal,
        workspace: Workspace,
        parent: SessionId,
        depth: u32,
        by: ClientId,
    ) -> Result<SessionId, HostError> {
        if depth > self.max_depth() {
            return Err(HostError::max_depth(self.max_depth()));
        }
        let id = journal.id();
        let resolved = ResolvedRef {
            id,
            workspace,
            depth,
            ephemeral: false,
            resumed: false,
            child: true,
            parent: Some(parent),
            name: None,
        };
        self.spawn_session(resolved, by, Some(journal), None)
            .await?;
        Ok(id)
    }

    /// Executes replay effects: emits persist before publish, the rest parks.
    async fn execute_replay(
        &self,
        journal: &mut Journal,
        shared: &Arc<Shared>,
        effects: Vec<Effect>,
        pending: &mut Vec<Effect>,
    ) -> Result<(), HostError> {
        for effect in effects {
            match effect {
                Effect::Emit(emit) => {
                    journal.append(emit.records).await?;
                    for kind in emit.updates {
                        shared.publish(kind);
                    }
                }
                Effect::Delta {
                    turn,
                    channel,
                    text,
                } => {
                    shared.publish(UpdateKind::Delta {
                        turn,
                        channel,
                        text,
                    });
                }
                Effect::Reply(_) => {}
                driver => pending.push(driver),
            }
        }
        Ok(())
    }

    fn store_for(&self, workspace: &Workspace) -> Store {
        Store::new(
            self.state.shared.data_root.clone(),
            workspace.clone(),
            journal_product(self.state.shared.product_name),
        )
    }

    /// Returns the child-session depth limit from the typed `[agents]` table.
    fn max_depth(&self) -> u32 {
        self.state.shared.config.agents().max_depth.get()
    }

    fn depth_of(&self, parent: SessionId) -> Result<u32, HostError> {
        self.state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&parent)
            .map(|entry| entry.depth)
            .ok_or_else(|| HostError::NotFound {
                message: format!("unknown session {parent}.").into(),
            })
    }

    fn take_entry(&self, id: SessionId) -> Result<SessionEntry, HostError> {
        self.state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
            .ok_or_else(|| HostError::NotFound {
                message: format!("unknown session {id}.").into(),
            })
    }

    fn table_ids(&self) -> Vec<SessionId> {
        self.state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .copied()
            .collect()
    }

    fn entries(&self) -> Vec<(SessionId, SessionPorts)> {
        self.state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(id, entry)| {
                (
                    *id,
                    SessionPorts {
                        id: *id,
                        handle: entry.handle.clone(),
                        shared: Arc::clone(&entry.shared),
                        broker: Arc::clone(&entry.broker),
                        workspace: entry.workspace.clone(),
                        generation: entry.generation,
                        control: Arc::clone(&entry.control),
                    },
                )
            })
            .collect()
    }

    fn workspaces(&self) -> Vec<Workspace> {
        let mut workspaces: Vec<Workspace> = self
            .entries()
            .into_iter()
            .map(|(_, ports)| ports.workspace)
            .collect();
        workspaces.dedup();
        workspaces
    }

    pub(crate) fn publish(&self, update: &HostUpdate) {
        self.state
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|sender| sender.send((*update).clone()).is_ok());
    }
}

async fn observe_session_start(
    generation: &Generation,
    services: &Arc<dyn Services>,
    cancel: &CancellationToken,
    parent: Option<SessionId>,
    process_env: &Arc<crate::Env>,
    script: Option<ScriptCx>,
    event: &SessionStart,
) {
    let mut report = ObserverReport::default();
    let scope = HookScope {
        services,
        session: event.session,
        parent,
        process_env: Arc::clone(process_env),
        cancel,
        turn_deadline: tokio::time::Instant::now() + Duration::from_secs(60),
        script,
    };
    for (index, extension, caller) in hook_fanout(generation, None) {
        let cx = scope.cx(&caller, None);
        let failed_before = report.failed;
        dispatch_session_start(
            extension.name(),
            &cx,
            generation.session_starts(index),
            event,
            &mut report,
        )
        .await;
        dispatch_session_start(
            extension.name(),
            &cx,
            generation.session_starts_lossless(index),
            event,
            &mut report,
        )
        .await;
        notify_observer_failure(services, &caller, "session_start", failed_before, &report);
    }
}

async fn observe_session_end(
    generation: &Generation,
    services: &Arc<dyn Services>,
    cancel: &CancellationToken,
    parent: Option<SessionId>,
    process_env: &Arc<crate::Env>,
    script: Option<ScriptCx>,
    event: &SessionEnd,
) {
    let mut report = ObserverReport::default();
    let scope = HookScope {
        services,
        session: event.session,
        parent,
        process_env: Arc::clone(process_env),
        cancel,
        turn_deadline: tokio::time::Instant::now() + Duration::from_secs(60),
        script,
    };
    for (index, extension, caller) in hook_fanout(generation, None) {
        let cx = scope.cx(&caller, None);
        let failed_before = report.failed;
        dispatch_session_end(
            extension.name(),
            &cx,
            generation.session_ends(index),
            event,
            &mut report,
        )
        .await;
        dispatch_session_end(
            extension.name(),
            &cx,
            generation.session_ends_lossless(index),
            event,
            &mut report,
        )
        .await;
        notify_observer_failure(services, &caller, "session_end", failed_before, &report);
    }
}

/// Maps a product name to its journal product.
fn journal_product(name: &str) -> dal_core::Product {
    if name == "dalgona" {
        dal_core::Product::Dalgona
    } else {
        dal_core::Product::Dal
    }
}

/// Snapshot arguments for listing rows of sessions missing from the store.
fn snapshot_args(entry: &SessionPorts) -> crate::session::projection::SnapshotArgs {
    crate::session::projection::SnapshotArgs {
        generation: entry.generation,
        id: entry.id,
        workspace: entry.workspace.clone(),
        open: entry.broker.open_requests(),
        updated_at: Timestamp::now(),
        created_at: None,
        archived: None,
        page: PageReq::default(),
    }
}
