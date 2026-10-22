//! Session table: open, close, shutdown, listing, and host updates.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use dal_core::{
    ApprovalMode, ClientId, Effect, EntryKind, Gen, ListQuery, ModelRoute, Name, Page, PageReq,
    Record, Session, SessionEnd, SessionId, SessionInfo, SessionStart, Timestamp, UpdateKind,
    Workspace,
};
use dal_store::{Journal, Store};
use serde::Deserialize;
use tokio::time::{timeout, timeout_at};
use tokio_util::sync::CancellationToken;

use super::{
    Host, HostState, HostSubscription, HostUpdate, NameClaim, SessionEntry, SessionRef,
    ShutdownReport,
};
use crate::agent::{Agent, AgentInner};
use crate::broker::Broker;
use crate::error::HostError;
use crate::ext::generation::Generation;
use crate::ext::grants::GrantStore;
use crate::ext::hooks::{
    HookScope, ObserverReport, dispatch_session_end, dispatch_session_start,
    hook_caller_with_continuation,
};
use crate::ext::script::ScriptCx;
use crate::ext::services::{SessionBackend, SessionServices, SessionServicesDeps};
use crate::ext::{Caller, Services};
use crate::session::SessionHandle;
use crate::session::actor::{ActorDeps, DriverPorts, spawn};
use crate::session::backend::{Backend, BackendDeps};
use crate::session::continuation::ContinuationOwner;
use crate::session::rt::{SessionRt, SessionRtDeps};
use crate::session::script::SessionScriptHost;
use crate::session::shared::ChildPromptPolicy;

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

/// The `[ask] timeout` of the host's validated configuration.
fn ask_timeout(config: &dal_core::Config) -> std::time::Duration {
    config.ask().timeout()
}

/// Publishes the one session-open notice for a clamped `[ask] timeout`.
fn notify_ask_clamp(config: &dal_core::Config, backend: &Backend) {
    if let Some(notice) = config.ask_clamp_notice() {
        backend.notify(dal_core::Notice {
            turn: None,
            kind: "ask".into(),
            text: notice.into(),
        });
    }
}

/// Sweep interval of the shutdown quiet wait; polls are bounded reads.
const STATUS_QUIET_POLL: Duration = Duration::from_millis(50);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildPolicyBody {
    tools: Option<Vec<Box<str>>>,
    approval: ApprovalMode,
    #[serde(default)]
    role: Option<Box<str>>,
    #[serde(default)]
    system: Option<Box<str>>,
}

struct ChildPolicy {
    tools: Option<Vec<Name>>,
    approval: ApprovalMode,
    role: Option<Box<str>>,
    system: Option<Box<str>>,
}

fn parse_child_policy_tool(name: &str) -> Result<Name, HostError> {
    Name::parse_mapped_tool(name).map_err(|error| HostError::Config {
        message: format!("invalid child policy tool {name:?}: {error}").into(),
    })
}

/// Reads the one host-owned child-policy record a journal carries; a journal
/// without one leaves the session unrestricted and on the configured mode.
/// The child-start path writes exactly one, so a second occurrence is a
/// forgery and fails the replay instead of letting the newest record win.
fn replay_child_policy(records: &[Record]) -> Result<Option<ChildPolicy>, HostError> {
    let mut found = records.iter().filter(|record| {
        matches!(
            record,
            Record::Ext { ext, kind, .. }
                if super::is_child_policy_record(ext.as_ref(), kind.as_ref())
        )
    });
    let Some(Record::Ext { body, .. }) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err(HostError::Config {
            message: "journal carries more than one child policy record".into(),
        });
    }
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
        role: body.role,
        system: body.system,
    }))
}

/// The route extension setup and the session's model identity start from:
/// the last `Model` record, else the route of the last response, else the
/// configured default.
///
/// A response records its API family and native model id but never its
/// provider, so a route rebuilt from it names none. The fold's requested
/// route is the configured default when no `Model` record exists; when it
/// names the same family and model as the response, it is that response's
/// route with its provider, and it is what the session's own turns send.
/// Setup consumers (reviewer, judge, history) then request through the
/// provider the session uses, not through a bare-id search that two
/// same-family providers make ambiguous. A response from a different model
/// keeps the provider-less route it recorded.
fn replayed_model(
    records: &[Record],
    fold: &Session,
    configured: Option<ModelRoute>,
) -> Option<ModelRoute> {
    replay_model(records)
        .or_else(|| {
            fold.last_response_model()
                .map(|last| match fold.requested_model() {
                    Some(requested) if requested.same_model(&last) => requested.clone(),
                    _ => last,
                })
        })
        .or(configured)
}

fn replay_model(records: &[Record]) -> Option<ModelRoute> {
    records.iter().rev().find_map(|record| match record {
        Record::Model(entry) => match &entry.kind {
            EntryKind::Model { route } => Some(route.clone()),
            _ => None,
        },
        _ => None,
    })
}
struct ReplayedSession {
    /// The route selected by the journal, when one exists.
    model: Option<ModelRoute>,
    /// The selected route's stable display identity.
    model_id: Box<str>,
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
    /// The route selected from replay or current configuration.
    model: Option<ModelRoute>,
    /// The selected model identity, empty when no model is available.
    model_id: Box<str>,
}

/// Owns a wired session until registry admission transfers its resources.
///
/// Host shutdown drains cleanup after an opener is cancelled or refused.
struct OpeningCleanup {
    state: Arc<HostState>,
    session: SessionId,
    parent: Option<SessionId>,
    wired: Option<WiredSession>,
    cancel: CancellationToken,
    child_tasks: SessionTasks,
    owner_tasks: SessionTasks,
    runtime: tokio::runtime::Handle,
}

impl OpeningCleanup {
    fn new(
        state: Arc<HostState>,
        session: SessionId,
        parent: Option<SessionId>,
        wired: WiredSession,
        cancel: CancellationToken,
        child_tasks: SessionTasks,
        owner_tasks: SessionTasks,
    ) -> Self {
        Self {
            state,
            session,
            parent,
            wired: Some(wired),
            cancel,
            child_tasks,
            owner_tasks,
            runtime: tokio::runtime::Handle::current(),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "the cleanup owns the wired session until activation or cleanup takes it once"
    )]
    fn wired(&self) -> &WiredSession {
        self.wired
            .as_ref()
            .expect("opening cleanup owns wired session until activation")
    }

    #[expect(
        clippy::expect_used,
        reason = "the cleanup owns the wired session until activation or cleanup takes it once"
    )]
    fn take_wired(&mut self) -> WiredSession {
        self.wired
            .take()
            .expect("opening cleanup owns wired session until activation")
    }

    /// Schedules the terminal session end plus full teardown on the
    /// host-owned task owner and returns a waiter for the caller. The task
    /// owns its own work: dropping the waiter never cancels the cleanup.
    fn schedule_cleanup(&mut self, end_reason: &str) -> tokio::sync::oneshot::Receiver<()> {
        let wired = self.take_wired();
        let session = self.session;
        let parent = self.parent;
        let cancel = self.cancel.clone();
        let child_tasks = self.child_tasks.clone();
        let generation = Arc::clone(&wired.ext_generation);
        let services: Arc<dyn Services> = wired.services.clone();
        let backend = Arc::clone(&wired.backend);
        let process_env = Arc::clone(&self.state.shared.env);
        let script = SessionScriptHost::for_generation(
            session,
            &backend,
            Arc::clone(&self.state.shared.interpreters),
            Arc::clone(&generation),
        )
        .attach(None);
        let end = SessionEnd {
            session,
            reason: end_reason.into(),
        };
        let runtime = self.runtime.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let end_tasks = self.owner_tasks.clone();
        self.owner_tasks.spawn_cleanup(&runtime, async move {
            observe_session_end(
                &generation,
                &services,
                &cancel,
                parent,
                &process_env,
                script,
                &end,
                end_tasks,
            )
            .await;
            // Classification first: the opening and running turn resolve
            // through the inbox before any lifecycle fire wakes holders.
            wired.handle.begin_shutdown();
            cancel.cancel();
            child_tasks.stop().await;
            wired.handle.shutdown().await;
            wired.overlay.clear();
            let _ = wired.task.await;
            let _ = done_tx.send(());
        });
        done_rx
    }
}

impl Drop for OpeningCleanup {
    fn drop(&mut self) {
        if self.wired.is_none() {
            return;
        }
        drop(self.schedule_cleanup("close"));
    }
}

/// The extension surface minted when a session is wired.
struct WiredExtensions {
    /// The grant store backing ask and run approvals.
    grants: Arc<GrantStore>,
    /// The extension overlay published for the session.
    overlay: Arc<crate::ext::overlay::Overlay>,
    /// The extension generation live at wire time.
    generation: Arc<Generation>,
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
        self.open_leaving(None, session, by).await
    }

    /// Opens the session that takes over from `leaving`, which the caller
    /// closes once the returned agent is attached.
    ///
    /// Behaves exactly like [`Host::open`], except that it refuses a target
    /// resolving to a live session the close of `leaving` would end.
    /// [`Host::close`] tears down the whole subtree it owns, so a successor
    /// bound to one of those actors would already be an ended session when
    /// the caller attached to it, and a caller that opens first and closes
    /// after loses nothing; ownership is never changed to avoid the refusal.
    /// A live ancestor of `leaving`, a live session outside its subtree, and
    /// a session that is not live all open as `open` opens them. A
    /// `SessionRef::Child` whose resolved parent is `leaving` or owned by it
    /// is refused the same way, since the new session would land in the
    /// subtree the close tears down.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Config`] when the target is `leaving`, resolves
    /// to a live session it owns, or would spawn under either of those, and
    /// otherwise the errors of [`Host::open`].
    pub async fn open_replacing(
        &self,
        leaving: SessionId,
        session: SessionRef,
        by: ClientId,
    ) -> Result<Agent, HostError> {
        self.open_leaving(Some(leaving), session, by).await
    }

    /// The one open path: `leaving` is the session the caller will close
    /// after attaching, when there is one. Only a target that is live in the
    /// table can share fate with `leaving`, so both live-binding branches
    /// check the id they resolved; a target that is not live spawns under its
    /// resolved parent, which is refused first when it is `leaving` or owned
    /// by it.
    async fn open_leaving(
        &self,
        leaving: Option<SessionId>,
        session: SessionRef,
        by: ClientId,
    ) -> Result<Agent, HostError> {
        if let SessionRef::Resume { key, .. } = &session
            && let Ok(id) = SessionId::parse(key)
            && let Some(entry) = self.entry_of(id)
        {
            self.refuse_ended_by(leaving, id)?;
            return Ok(Self::bind(&entry, id, by));
        }
        let resolved = self.resolve_ref(&session)?;
        if let Some(entry) = self.entry_of(resolved.id) {
            self.refuse_ended_by(leaving, resolved.id)?;
            return Ok(Self::bind(&entry, resolved.id, by));
        }
        if let Some(parent) = resolved.parent {
            self.refuse_ended_by(leaving, parent)?;
        }
        let name_claim = self.claim_name(&resolved)?;
        self.spawn_session(resolved, by, None, name_claim).await
    }

    /// Refuses a live `target` that closing `leaving` would end.
    fn refuse_ended_by(
        &self,
        leaving: Option<SessionId>,
        target: SessionId,
    ) -> Result<(), HostError> {
        let Some(leaving) = leaving else {
            return Ok(());
        };
        if target == leaving {
            return Err(HostError::Config {
                message: format!("session {target} is the session being left").into(),
            });
        }
        let owned = {
            let sessions = self
                .state
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::owned_by(&sessions, leaving, target)
        };
        if owned {
            return Err(HostError::Config {
                message: format!(
                    "session {target} runs under session {leaving} and would end with it"
                )
                .into(),
            });
        }
        Ok(())
    }

    /// Flushes one session, stops its actor, removes it from the table,
    /// and closes every live session-owned descendant first.
    ///
    /// The teardown runs on a host-owned task from the moment the entry
    /// leaves the table, so dropping the returned future, as a shutdown's
    /// close grace does, never abandons it: the drop only stops the wait
    /// and hurries the teardown past its settlement and end-hook waits,
    /// and cancellation, driver abort, actor shutdown, and the removal
    /// publication still run. Host shutdown drains these tails.
    ///
    /// # Errors
    ///
    /// Returns an error when `id` is not open or the host's runtime stops
    /// before the teardown finishes.
    pub async fn close(&self, id: SessionId) -> Result<(), HostError> {
        let entry = self.take_entry(id)?;
        let state = Arc::clone(&self.state);
        let hurry = CancellationToken::new();
        let hurry_on_drop = hurry.clone().drop_guard();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        self.state.session_teardowns.spawn_cleanup(
            &tokio::runtime::Handle::current(),
            async move {
                Self::teardown_subtree_hurried(&state, entry, id, &hurry).await;
                let _ = done_tx.send(());
            },
        );
        let done = done_rx.await;
        hurry_on_drop.disarm();
        done.map_err(|_| HostError::Closed)
    }

    /// Tears down one already-removed session together with every live
    /// session-owned descendant, every child before its parent.
    ///
    /// One subtree operation for both teardown owners: the explicit close
    /// and the abandoned-await tail. The sweep runs after the root entry
    /// is removed, and admission refuses any spawn whose owning chain is
    /// broken, so the listing cannot miss a member of the subtree; an
    /// entry already reclaimed elsewhere — an explicit `agents.cancel` or
    /// a competing sweep — is skipped, so exactly one teardown wins per
    /// id. Each descendant is fully torn down before its parent's
    /// teardown starts, so no end hook can wait on a live owner.
    pub(crate) async fn teardown_subtree(state: &HostState, entry: SessionEntry, id: SessionId) {
        Self::teardown_subtree_hurried(state, entry, id, &CancellationToken::new()).await;
    }

    /// The subtree teardown of [`Self::teardown_subtree`] whose settlement
    /// and end-hook waits end as soon as `hurry` fires; the cancellation,
    /// driver abort, and actor shutdown of every member always run.
    async fn teardown_subtree_hurried(
        state: &HostState,
        entry: SessionEntry,
        id: SessionId,
        hurry: &CancellationToken,
    ) {
        for child in Self::descendants_of(&state.sessions, id) {
            let child_entry = state
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&child);
            let Some(child_entry) = child_entry else {
                continue;
            };
            Self::teardown_taken(state, child_entry, child, hurry).await;
        }
        Self::teardown_taken(state, entry, id, hurry).await;
    }

    /// Tears down one session whose table entry is already removed: end
    /// hooks, cancellation, actor join, provider session-state eviction,
    /// then the removal publication.
    ///
    /// The removal happened before this runs, so the parent-scoped child
    /// list and child admission observe the reclaim before any teardown
    /// step; the publication last keeps subscriber evidence ordered after
    /// the teardown it reports. Only the settlement wait and the end-hook
    /// wait yield to `hurry`: everything after them is unconditional, and
    /// the caller runs this on a task its own cancellation cannot reach.
    /// An abandoned child await reuses this path through its await owner's
    /// drop and the shared subtree teardown, parking the tail on the
    /// awaiting session's task owner, which its own close and the host
    /// shutdown always drain.
    async fn teardown_taken(
        state: &HostState,
        mut entry: SessionEntry,
        id: SessionId,
        hurry: &CancellationToken,
    ) {
        // Wait for the last ended turn's driver confirmation before the
        // session-end observers sweep per-turn state: turn-end hooks journal
        // their records from the driver's turn-end observation, and tearing
        // down first drops those records (a turn-end hook races the close).
        // Bounded well under one hook chain; driver-gone closes resolve on
        // the actor shutdown instead.
        const SETTLE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
        tokio::select! {
            biased;
            () = hurry.cancelled() => {}
            _ = tokio::time::timeout(SETTLE_WAIT, entry.handle.settled()) => {}
        }
        let generation = state.shared.generation.borrow().clone();
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
            Arc::clone(&state.shared.interpreters),
            Arc::clone(&generation),
        )
        .attach(None);
        let process_env = Arc::clone(&state.shared.env);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let end_tasks = tasks.clone();
        tasks.spawn(async move {
            observe_session_end(
                &generation,
                &services,
                &cancel,
                entry_parent,
                &process_env,
                script,
                &event,
                end_tasks,
            )
            .await;
            let _ = done_tx.send(());
        });
        tokio::select! {
            biased;
            () = hurry.cancelled() => {}
            _ = done_rx => {}
        }
        // Classification first: the opening and running turn resolve
        // through the inbox before the session token or the abort wakes
        // holders into withdrawing as the wrong cause.
        entry.handle.begin_shutdown();
        entry.cancel.cancel();
        entry.driver.abort();
        entry.handle.shutdown().await;
        entry.overlay.clear();
        if let Some(task) = entry.task.take() {
            let _ = task.await;
        }
        entry.tasks.stop().await;
        // Provider session state clears only after every producer is
        // drained: end hooks completed, the actor joined, the driver
        // aborted, and session tasks stopped, so no turn, request, or
        // parked workflow can put rows back afterwards. Clearing earlier
        // would let a still-driving turn resurrect its rows.
        state.shared.providers.end_session(&id);
        state.publish(&HostUpdate::SessionRemoved { session: id });
    }

    /// Lists the live descendants of `root`, every child before its
    /// parent, in one table pass. Sessions closed earlier are absent, and
    /// admission refuses any spawn whose owning chain is broken, so this
    /// listing cannot miss a member of the closing subtree.
    fn descendants_of(
        sessions: &std::sync::Mutex<HashMap<SessionId, SessionEntry>>,
        root: SessionId,
    ) -> Vec<SessionId> {
        let sessions = sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut children: HashMap<SessionId, Vec<SessionId>> = HashMap::new();
        for (id, entry) in sessions.iter() {
            if let Some(parent) = entry.parent {
                children.entry(parent).or_default().push(*id);
            }
        }
        let mut ordered = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if let Some(kids) = children.remove(&node) {
                stack.extend(kids);
            }
            if node != root {
                ordered.push(node);
            }
        }
        ordered.reverse();
        ordered
    }

    /// Closes every session within one shared close grace and reports the
    /// count: the whole close phase — every open session, plus every
    /// descendant swept by its owner — spends at most `grace` of elapsed
    /// time, never `grace` per session.
    pub async fn shutdown(self, grace: Duration) -> ShutdownReport {
        self.shutdown_with_status_wait(Some(grace), grace).await
    }

    /// Closes every session after the caller has completed the quiet wait.
    ///
    /// Use this when a front end already waited for status kinds and must not
    /// spend the same status grace a second time.
    ///
    /// `close_grace` is one shared budget for every close, not a per-session
    /// timeout. The trailing provider shutdown runs after the closes without
    /// a timeout: durable provider completion can exceed `close_grace`.
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
        let session_teardowns = self.state.session_teardowns.clone();
        let ids: Vec<SessionId> = self.table_ids();
        let task_owners: Vec<_> = self
            .state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|entry| entry.tasks.clone())
            .collect();
        // One monotonic deadline shared by every close: a blocked session
        // spends only the budget left on this clock, so the whole close
        // phase stays inside `close_grace` no matter how many sessions are
        // open. A close the clock outlives is dropped mid-teardown exactly
        // like a timed-out close: its entry tail stays parked on its task
        // owner and is counted in `tasks_remaining`.
        let close_deadline = tokio::time::Instant::now() + close_grace;
        for id in &ids {
            let this = self.clone();
            let _ = timeout_at(close_deadline, this.close(*id)).await;
        }
        // A close dropped at its grace left its hurried teardown on the
        // host-owned owner; draining it first means every snapshot id that
        // left the table has run its whole teardown, directly or as a
        // descendant swept by its owner's close. Ids are unique, so a swept
        // member cannot double-report.
        session_teardowns.stop().await;
        let live: HashSet<SessionId> = self.table_ids().into_iter().collect();
        let sessions_closed = ids.iter().filter(|id| !live.contains(id)).count();
        // Provider shutdown joins its internally bounded refresh tasks. Do not
        // wrap this await in `close_grace`: dropping that waiter would detach
        // a credential write that can still commit after host shutdown.
        self.state.shared.providers.shutdown().await;
        let tasks_remaining = task_owners.iter().fold(0_usize, |total, tasks| {
            total.saturating_add(tasks.tracked())
        }) + session_teardowns.tracked();
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
            rows.entry(id).or_insert_with(|| live_row(&entry));
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

    /// Resolves one session reference across the host's known workspaces with
    /// no listing cap, and returns its listing row.
    ///
    /// `reference` is a full id, an exact name, or an id prefix, resolved by
    /// each workspace store exactly as `--resume` resolves it
    /// ([`Store::resolve`]); a full id also finds a session that is live in
    /// this host without a journal on disk (an ephemeral one). The row's
    /// workspace is the workspace the session lives in, so a caller reopens it
    /// there instead of guessing one. The workspaces searched are the ones
    /// [`Host::sessions`] lists from, so a session that listing shows is one
    /// this finds, however far down the listing it sits.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::NotFound`] when no known workspace holds a match,
    /// and the store error for an empty or ambiguous reference or an
    /// unreadable workspace.
    pub fn find_session(&self, reference: &str) -> Result<SessionInfo, HostError> {
        let reference = reference.trim();
        let exact = SessionId::parse(reference).ok();
        if let Some(entry) = exact.and_then(|id| self.entry_of(id)) {
            return Ok(live_row(&entry));
        }
        for workspace in self.workspaces() {
            let store = self.store_for(&workspace);
            if let Some(id) = resolve_in(&store, &workspace, reference, exact)? {
                return store.info(id).map_err(HostError::from);
            }
        }
        Err(HostError::NotFound {
            message: format!("no session matches \"{reference}\".").into(),
        })
    }

    /// Deletes one session tree through the store's own ownership rules.
    ///
    /// A session that is live in this host's table is refused without
    /// touching its tree. For every other id each known workspace store
    /// attempts [`Store::delete`], whose session lock is the second refusal:
    /// a tree another process (or a closing actor) still holds fails as
    /// [`HostError::SessionBusy`]. The workspaces are the ones
    /// [`Host::sessions`] lists from, so a row the host listed is a row it
    /// can delete; no fallback workspace is invented for an unknown id.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Config`] when the session is live here,
    /// [`HostError::SessionBusy`] when its lock is held, [`HostError::NotFound`]
    /// when no known workspace holds it, and the store error otherwise.
    pub fn delete_session(&self, id: SessionId) -> Result<(), HostError> {
        if self.table_ids().contains(&id) {
            return Err(HostError::Config {
                message: format!("session {id} is active; switch away before deleting it.").into(),
            });
        }
        for workspace in self.workspaces() {
            match self.store_for(&workspace).delete(id) {
                Ok(()) => return Ok(()),
                Err(dal_store::StoreError::NotFound { .. }) => {}
                Err(error) => return Err(HostError::from(error)),
            }
        }
        Err(HostError::NotFound {
            message: format!("unknown session {id}.").into(),
        })
    }

    /// Registers a host lifecycle subscription.
    #[must_use]
    pub fn subscribe(&self) -> HostSubscription {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let pending_changed =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        self.state
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(super::Subscriber {
                sender: tx,
                pending_changed: pending_changed.clone(),
            });
        HostSubscription {
            receiver: rx,
            pending_changed,
        }
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

/// Generation snapshots shared by every per-extension session setup.
struct SetupInputs<'a> {
    resolved: &'a ResolvedRef,
    wired: &'a WiredSession,
    cancel: &'a CancellationToken,
    config: &'a Arc<dal_core::Config>,
    rules: &'a [crate::ext::SessionRule],
    tool_names: &'a [Box<str>],
    rule_files: &'a [dal_core::RuleFile],
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
        let configured_model = self.state.shared.config.model().map(ModelRoute::from_id);
        let (fold, effects) = Session::replay_with(
            dal_core::Settings {
                model: configured_model.clone(),
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
        let model = replayed_model(journal.records(), &fold, configured_model);
        let model_id = model
            .as_ref()
            .map_or_else(Box::default, |route| route.id().into());
        let shared = Arc::new(Shared::new(
            resolved.child,
            generation,
            thinking,
            approval,
            mode,
        ));
        shared.restore_fold(&fold);
        if let Some(policy) = &policy {
            if let Some(tools) = &policy.tools {
                shared.restrict_tools(tools);
            }
            if let Some(system) = &policy.system {
                shared.set_child_prompt_policy(ChildPromptPolicy::System(system.clone()));
            } else if let Some(role) = &policy.role {
                shared.set_child_prompt_policy(ChildPromptPolicy::Role(role.clone()));
            }
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
            model,
            model_id,
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
    fn wire_session(
        &self,
        resolved: &ResolvedRef,
        journal: Journal,
        replayed: ReplayedSession,
        cancel: &CancellationToken,
        tasks: &SessionTasks,
    ) -> Result<WiredSession, HostError> {
        let ReplayedSession {
            model,
            model_id,
            fold,
            generation,
            shared,
            initial_entries,
            opening_texts,
            pending,
            jobs,
        } = replayed;
        let broker = Arc::new(Broker::new());
        let extensions = self.grants_and_overlay(&broker, cancel, resolved.workspace.as_path())?;
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
        let turn_control = Arc::clone(&ports.control);
        let services = Arc::new(SessionServices::new(SessionServicesDeps {
            grants: extensions.grants,
            broker: Arc::clone(&broker),
            backend: backend.clone(),
            rt,
            mcp_client: extensions
                .generation
                .mcp()
                .map(|(_, client)| Arc::clone(client)),
            generation: self.state.shared.generation.subscribe(),
            overlay: Arc::clone(&extensions.overlay),
            history: opening_texts,
            sites: HashMap::new(),
            turn_cancel: Arc::new(move |turn| {
                turn_control
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .token_for(turn)
            }),
            cancel: cancel.clone(),
            resolutions: handle.resolutions(),
            ask_timeout: ask_timeout(&self.state.shared.config),
            ephemeral: resolved.ephemeral,
            workspace: resolved.workspace.clone(),
        }));
        notify_ask_clamp(&self.state.shared.config, &backend);
        backend.set_services(&services);
        Ok(WiredSession {
            handle,
            ports,
            task,
            backend,
            services,
            overlay: extensions.overlay,
            ext_generation: extensions.generation,
            generation,
            broker,
            shared,
            jobs,
            model,
            model_id,
        })
    }

    /// Loads the grant store, extension overlay, and live generation for a
    /// session being wired.
    fn grants_and_overlay(
        &self,
        broker: &Arc<Broker>,
        cancel: &CancellationToken,
        workspace: &std::path::Path,
    ) -> Result<WiredExtensions, HostError> {
        let grants = Arc::new(
            GrantStore::with_runtime_scoped(
                self.state.shared.data_root.clone(),
                workspace.to_path_buf(),
                ask_timeout(&self.state.shared.config),
                Arc::clone(broker),
            )
            .map_err(|error| HostError::Config {
                message: error.to_string().into(),
            })?,
        );
        let overlay = Arc::new(crate::ext::overlay::Overlay::with_grants(
            Arc::clone(&grants),
            cancel.clone(),
        ));
        let ext_generation = self.state.shared.generation.borrow().clone();
        Ok(WiredExtensions {
            grants,
            overlay,
            generation: ext_generation,
        })
    }

    /// Fires the `session_start` observer for the wired session.
    async fn observe_start(
        &self,
        resolved: &ResolvedRef,
        wired: &WiredSession,
        cancel: &CancellationToken,
        resumed: bool,
        tasks: &SessionTasks,
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
            tasks.clone(),
        )
        .await;
    }

    /// Spawns the driver, registers the session entry, and publishes the
    /// host update; returns the bound agent.
    ///
    /// Registration commits only while the whole owning chain is still
    /// live: a close takes the root entry before it tears the subtree
    /// down, so a spawn racing that close unwinds here instead of
    /// committing a session nobody will reap.
    async fn activate_session(
        &self,
        resolved: &ResolvedRef,
        opening: &mut OpeningCleanup,
        cancel: CancellationToken,
        tasks: &SessionTasks,
        by: ClientId,
        name_claim: Option<NameClaim>,
    ) -> Result<Agent, HostError> {
        let id = resolved.id;
        let parent = {
            let mut sessions = self
                .state
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let refused = resolved
                .parent
                .filter(|parent| !Self::chain_live(&sessions, *parent));
            let Some(parent) = refused else {
                let wired = opening.take_wired();
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
                    control,
                };
                let agent = Self::bind(&ports, id, by);
                sessions.insert(
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
                        task: Some(wired.task),
                        control: ports.control,
                        cancel,
                        backend: Arc::clone(&wired.backend),
                        overlay: wired.overlay,
                        reported: std::sync::atomic::AtomicBool::new(false),
                        prompted: std::sync::atomic::AtomicBool::new(false),
                    },
                );
                drop(sessions);
                if let Some(parent) = resolved.parent {
                    self.publish(&HostUpdate::ChildStarted {
                        parent,
                        child: id,
                        name: String::new().into(),
                    });
                } else {
                    self.publish(&HostUpdate::SessionAdded { session: id });
                }
                return Ok(agent);
            };
            parent
        };
        let done = opening.schedule_cleanup("parent closed while opening");
        let _ = done.await;
        Err(HostError::NotFound {
            message: format!("parent session {parent} closed while opening {id}.").into(),
        })
    }

    /// Whether `id` is a live session owned by `owner`, directly or through
    /// other live sessions: the relation `descendants_of` lists for a
    /// teardown, read from the member's side as a walk up its parent links,
    /// which are acyclic.
    fn owned_by(
        sessions: &HashMap<SessionId, SessionEntry>,
        owner: SessionId,
        id: SessionId,
    ) -> bool {
        let mut cursor = sessions.get(&id).and_then(|entry| entry.parent);
        while let Some(parent) = cursor {
            if parent == owner {
                return true;
            }
            cursor = sessions.get(&parent).and_then(|entry| entry.parent);
        }
        false
    }

    /// Whether every ancestor from `parent` to its root is still a live
    /// table entry. Parent links are acyclic: a child entry is created
    /// strictly after its parent's.
    fn chain_live(sessions: &HashMap<SessionId, SessionEntry>, parent: SessionId) -> bool {
        let mut cursor = Some(parent);
        while let Some(id) = cursor {
            let Some(entry) = sessions.get(&id) else {
                return false;
            };
            cursor = entry.parent;
        }
        true
    }

    /// Runs one extension's fallible session setup and installs its result.
    ///
    /// Extensions without a setup pass through; a cancelled, failed, or
    /// duplicate setup fails the session open with `HostError::Config`.
    async fn run_one_session_setup(
        &self,
        extension: &crate::ext::Extension,
        inputs: &SetupInputs<'_>,
    ) -> Result<(), HostError> {
        let SetupInputs {
            resolved,
            wired,
            cancel,
            config,
            rules,
            tool_names,
            rule_files,
        } = inputs;
        let Some(setup) = extension.session_setup() else {
            return Ok(());
        };
        let owner = extension
            .name()
            .parse::<Name>()
            .map_err(|error| HostError::Config {
                message: error.to_string().into(),
            })?;
        let inherited_state = resolved
            .parent
            .and_then(|parent| self.entry_of(parent))
            .and_then(|entry| entry.shared.owner_state(&owner));
        let setup_owner = ContinuationOwner::new(wired.backend.tasks().clone());
        let caller = Caller::new(
            owner.clone(),
            extension.origin(),
            extension.inject(),
            extension.state_version(),
            crate::ext::CallerKind::Hook,
            None,
        )
        .with_extension(extension)
        .with_continuation(setup_owner.downgrade());
        let cx = crate::ext::SessionCx {
            session: resolved.id,
            workspace: resolved.workspace.clone(),
            parent: resolved.parent,
            product: self.state.shared.product_name,
            config: Arc::clone(config),
            services: wired.services.clone(),
            caller,
            cancel: (*cancel).clone(),
            data_root: self.state.shared.data_root.clone(),
            rules: rules.to_vec(),
            tool_names: tool_names.to_vec(),
            rule_files: rule_files.to_vec(),
            model: wired.model.clone(),
            model_id: wired.model_id.clone(),
            inherited_state,
        };
        let installs = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                setup_owner.release_if_live();
                return Err(HostError::Config { message: "session setup cancelled".into() });
            }
            result = timeout(Duration::from_millis(crate::ext::hooks::HOOK_DEADLINE_MS), setup.open(cx)) => {
                let installs = match result {
                    Ok(Ok(installs)) => installs,
                    Ok(Err(error)) => {
                        setup_owner.release_if_live();
                        return Err(HostError::Config { message: error.to_string().into() });
                    }
                    Err(_) => {
                        setup_owner.release_if_live();
                        return Err(HostError::Config { message: "session setup deadline exceeded".into() });
                    }
                };
                setup_owner.release_if_live();
                installs
            }
        };
        if cancel.is_cancelled() {
            return Err(HostError::Config {
                message: "session setup cancelled".into(),
            });
        }
        wired
            .shared
            .install_setup(owner, installs)
            .map_err(|()| HostError::Config {
                message: "duplicate session setup owner".into(),
            })?;
        Ok(())
    }

    async fn run_session_setups(
        &self,
        resolved: &ResolvedRef,
        wired: &WiredSession,
        cancel: &CancellationToken,
    ) -> Result<(), HostError> {
        let generation = &wired.ext_generation;
        let config = Arc::new(self.state.shared.config.clone());
        let mut rules = Vec::new();
        let mut tool_names = Vec::new();
        for extension in generation.extensions.iter() {
            let owner = extension
                .name()
                .parse::<Name>()
                .map_err(|error| HostError::Config {
                    message: error.to_string().into(),
                })?;
            rules.extend(
                extension
                    .rules()
                    .iter()
                    .cloned()
                    .map(|record| crate::ext::SessionRule {
                        ext: owner.clone(),
                        site: extension.site().cloned(),
                        record,
                    }),
            );
            tool_names.extend(
                extension
                    .tools()
                    .iter()
                    .map(|(tool, _)| tool.name().as_str().into()),
            );
        }
        let rule_files = generation.rule_files().to_vec();
        let inputs = SetupInputs {
            resolved,
            wired,
            cancel,
            config: &config,
            rules: &rules,
            tool_names: &tool_names,
            rule_files: &rule_files,
        };
        for extension in generation.extensions.iter() {
            self.run_one_session_setup(extension, &inputs).await?;
        }
        Ok(())
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
        let wired = self.wire_session(&resolved, journal, replayed, &cancel, &tasks)?;
        let setup = async {
            wired
                .handle
                .services(wired.services.clone())
                .await
                .map_err(|error| HostError::Config {
                    message: error.to_string().into(),
                })?;
            self.run_session_setups(&resolved, &wired, &cancel).await
        };
        if let Err(error) = setup.await {
            // A setup that failed after opening a request still classifies
            // it before the lifecycle fires, so the drain journals it.
            wired.handle.begin_shutdown();
            cancel.cancel();
            tasks.stop().await;
            wired.handle.shutdown().await;
            wired.overlay.clear();
            let _ = wired.task.await;
            return Err(error);
        }
        let owner_tasks = self.state.session_teardowns.clone();
        let mut opening = OpeningCleanup::new(
            Arc::clone(&self.state),
            resolved.id,
            resolved.parent,
            wired,
            cancel.clone(),
            tasks.clone(),
            owner_tasks,
        );
        self.observe_start(&resolved, opening.wired(), &cancel, resumed, &tasks)
            .await;
        self.activate_session(&resolved, &mut opening, cancel, &tasks, by, name_claim)
            .await
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
        self.state.publish(update);
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
    tasks: SessionTasks,
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
    for (index, extension) in generation.extensions.iter().enumerate() {
        // One linear owner per hook iteration (decision b): session-start
        // observers run sequentially and each can reach model inference.
        let hook_owner = ContinuationOwner::new(tasks.clone());
        let Some(hook_caller) =
            hook_caller_with_continuation(extension, None, Some(hook_owner.downgrade()))
        else {
            hook_owner.release_if_live();
            continue;
        };
        let cx = scope.cx(&hook_caller, None);
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
        notify_observer_failure(
            services,
            &hook_caller,
            "session_start",
            failed_before,
            &report,
        );
        hook_owner.release_if_live();
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
    tasks: SessionTasks,
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
    for (index, extension) in generation.extensions.iter().enumerate() {
        // One linear owner per hook iteration (decision b): session-end
        // observers run sequentially and each can reach model inference.
        let hook_owner = ContinuationOwner::new(tasks.clone());
        let Some(hook_caller) =
            hook_caller_with_continuation(extension, None, Some(hook_owner.downgrade()))
        else {
            hook_owner.release_if_live();
            continue;
        };
        let cx = scope.cx(&hook_caller, None);
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
        notify_observer_failure(
            services,
            &hook_caller,
            "session_end",
            failed_before,
            &report,
        );
        hook_owner.release_if_live();
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

/// The listing row of a session that is live in this host. A live session
/// with no stored row (an ephemeral one) counts its own leaf path, the same
/// length the store folds for a stored row.
fn live_row(entry: &SessionPorts) -> SessionInfo {
    let mut session = entry.shared.snapshot(snapshot_args(entry)).session;
    session.message_count = u64::try_from(entry.shared.leaf_entries().len()).ok();
    session
}

/// Resolves `reference` in one workspace store: a full id by its journal file
/// (no workspace read), anything else through the store's own resolution.
/// `Ok(None)` means this workspace holds no match.
fn resolve_in(
    store: &Store,
    workspace: &Workspace,
    reference: &str,
    exact: Option<SessionId>,
) -> Result<Option<SessionId>, HostError> {
    if let Some(id) = exact {
        return Ok(store.session_file(id).is_file().then_some(id));
    }
    match store.resolve(workspace, reference) {
        Ok(id) => Ok(Some(id)),
        Err(dal_store::StoreError::NoMatch { .. }) => Ok(None),
        Err(error) => Err(HostError::from(error)),
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

#[cfg(test)]
mod close_tests;

#[cfg(test)]
mod tests {
    use dal_core::{RawJson, Record, Timestamp};

    use super::replay_child_policy;
    use std::collections::{BTreeMap, HashSet};
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use dal_core::{ClientId, Config, ConfigProduct, ServiceSet, SessionEnd, SessionId, Workspace};
    use tokio::time::Instant;

    use crate::agent::Agent;
    use crate::ext::{BoxFuture, Extension, ExtensionBuilder, HookCx, HookError, ObserveHook};
    use crate::{Env, Host, HostUpdate, Product, SessionRef};

    fn policy_record(ext: &str, kind: &str, body: &str) -> Record {
        Record::Ext {
            at: Timestamp::UNIX_EPOCH,
            ext: ext.into(),
            kind: kind.into(),
            body: RawJson::parse(body).expect("record body parses"),
        }
    }

    #[test]
    fn a_single_policy_record_replays() {
        let records = [policy_record(
            "dal-agent",
            "child_policy",
            r#"{"tools":["read_file"],"approval":"ask"}"#,
        )];
        let policy = replay_child_policy(&records)
            .expect("policy replays")
            .expect("policy present");
        assert_eq!(policy.tools.map(|tools| tools.len()), Some(1));
    }

    #[test]
    fn a_second_policy_record_fails_the_replay() {
        let records = [
            policy_record(
                "dal-agent",
                "child_policy",
                r#"{"tools":["read_file"],"approval":"ask"}"#,
            ),
            policy_record(
                "dal-agent",
                "child_policy",
                r#"{"tools":null,"approval":"all"}"#,
            ),
        ];
        assert!(replay_child_policy(&records).is_err());
    }

    #[test]
    fn other_records_leave_the_session_unrestricted() {
        let records = [policy_record("focus", "child_policy", "{}")];
        assert!(replay_child_policy(&records).expect("replays").is_none());
    }

    // ---- shutdown close-budget fixtures ------------------------------------

    /// Bound for settle waits in the shutdown fixtures; failures assert.
    const WAIT: Duration = Duration::from_secs(30);
    /// Poll step for flag checks; the paused test clock advances it freely.
    const POLL: Duration = Duration::from_millis(10);

    const SCRIPT_END_LINE: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

    /// Session-end observer used to block or pace closes under controlled
    /// time. The n-th entered hook records its boundary, waits out `hold` of
    /// virtual time, then waits on the n-th gate signal. Closes are
    /// sequential inside the shutdown loop, so entry order is close order.
    struct GatedEnd {
        log: Arc<Mutex<Vec<(SessionId, String)>>>,
        gates: Arc<Mutex<Vec<Option<tokio::sync::oneshot::Receiver<()>>>>>,
        entered: Arc<AtomicUsize>,
        cancelled: Arc<AtomicUsize>,
        released: Vec<Arc<AtomicBool>>,
        hold: Duration,
    }

    struct EndGuard {
        cancelled: Arc<AtomicUsize>,
        completed: bool,
    }

    impl Drop for EndGuard {
        fn drop(&mut self) {
            if !self.completed {
                self.cancelled.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    impl ObserveHook<SessionEnd> for GatedEnd {
        fn call(
            &self,
            input: SessionEnd,
            _cx: HookCx,
        ) -> BoxFuture<'static, Result<(), HookError>> {
            let log = Arc::clone(&self.log);
            let gates = Arc::clone(&self.gates);
            let entered = Arc::clone(&self.entered);
            let cancelled = Arc::clone(&self.cancelled);
            let released = self.released.clone();
            let hold = self.hold;
            Box::pin(async move {
                let index = entered.fetch_add(1, Ordering::SeqCst);
                let mut end_guard = EndGuard {
                    cancelled,
                    completed: false,
                };
                log.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((input.session, input.reason.into()));
                if hold > Duration::ZERO {
                    tokio::time::sleep(hold).await;
                }
                let receiver = gates
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)[index]
                    .take();
                if let Some(receiver) = receiver {
                    let _ = receiver.await;
                }
                end_guard.completed = true;
                released[index].store(true, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    fn gated_end_extension(
        log: Arc<Mutex<Vec<(SessionId, String)>>>,
        gates: Arc<Mutex<Vec<Option<tokio::sync::oneshot::Receiver<()>>>>>,
        entered: Arc<AtomicUsize>,
        cancelled: Arc<AtomicUsize>,
        released: Vec<Arc<AtomicBool>>,
        hold: Duration,
    ) -> Extension {
        ExtensionBuilder::new("shutdown_clock", "0.1.0", ServiceSet::EMPTY)
            .expect("extension builder")
            .on_session_end(GatedEnd {
                log,
                gates,
                entered,
                cancelled,
                released,
                hold,
            })
            .build()
            .expect("extension")
    }

    struct ShutdownRig {
        _tmp: tempfile::TempDir,
        host: Host,
        _agents: Vec<Agent>,
    }

    /// Starts a real host on the scripted provider with `sessions` open
    /// sessions and no live turns, so shutdown exercises exactly the close
    /// path under test.
    async fn shutdown_rig(extension: Extension, sessions: usize) -> ShutdownRig {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        let workspace_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&data).expect("data directory");
        std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
        let fixture = data.join("script.jsonl");
        std::fs::write(&fixture, SCRIPT_END_LINE).expect("script fixture");
        let config_text = format!(
            "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
            fixture.to_string_lossy().replace('\\', "\\\\")
        );
        let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str()))
            .expect("config");
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
        let host = Host::start(product, config, env).await.expect("host");
        let mut agents = Vec::new();
        for index in 0..sessions {
            let workspace = Workspace::new(workspace_dir.clone()).expect("workspace");
            agents.push(
                host.open(
                    SessionRef::New {
                        workspace,
                        name: None,
                    },
                    ClientId::new(format!("shutdown-clock-{index}")),
                )
                .await
                .expect("session"),
            );
        }
        ShutdownRig {
            _tmp: tmp,
            host,
            _agents: agents,
        }
    }

    /// The send halves and shared gate halves of the shutdown test rig.
    type GateChannels = (
        Vec<tokio::sync::oneshot::Sender<()>>,
        Arc<Mutex<Vec<Option<tokio::sync::oneshot::Receiver<()>>>>>,
    );

    fn gate_channels(count: usize) -> GateChannels {
        let (senders, receivers): (Vec<_>, Vec<_>) = (0..count)
            .map(|_| tokio::sync::oneshot::channel::<()>())
            .unzip();
        let gates = Arc::new(Mutex::new(
            receivers.into_iter().map(Some).collect::<Vec<_>>(),
        ));
        (senders, gates)
    }

    fn release_flags(count: usize) -> Vec<Arc<AtomicBool>> {
        (0..count)
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect()
    }

    fn recorded_ends(log: &Arc<Mutex<Vec<(SessionId, String)>>>) -> Vec<(SessionId, String)> {
        log.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn wait_for(description: &str, mut settled: impl FnMut() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !settled() {
            assert!(Instant::now() < deadline, "never {description}");
            tokio::time::sleep(POLL).await;
        }
    }

    /// Three sessions whose end hooks block on held gates: one shared
    /// deadline bounds the whole close phase to a single grace (a fresh
    /// per-session timeout would cost one grace per session), the blocked
    /// tails stay parked and reported rather than detached, and every
    /// entered hook is accounted for when cancellation drops its future.
    #[tokio::test(start_paused = true)]
    async fn one_deadline_bounds_the_whole_close_phase_with_blocked_sessions() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicUsize::new(0));
        let released = release_flags(3);
        let (senders, gates) = gate_channels(3);
        let rig = shutdown_rig(
            gated_end_extension(
                Arc::clone(&log),
                gates,
                Arc::clone(&entered),
                Arc::clone(&cancelled),
                released.clone(),
                Duration::ZERO,
            ),
            3,
        )
        .await;

        let grace = Duration::from_millis(500);
        let began = Instant::now();
        let report = rig.host.clone().shutdown_after_quiet_wait(grace).await;
        let elapsed = began.elapsed();

        assert!(
            elapsed >= grace,
            "the shared deadline must be reached while closes block: {elapsed:?}"
        );
        assert!(
            elapsed < grace * 2,
            "the close phase cost more than one shared grace (a fresh \
             per-session timeout costs one grace per session): {elapsed:?}"
        );
        assert!(
            report.sessions_closed >= 1,
            "the first close always runs to the deadline: {report:?}"
        );
        assert!(
            report.tasks_remaining >= 1,
            "blocked end tails are reported as pending work, never detached: {report:?}"
        );
        let ends = recorded_ends(&log);
        assert!(
            !ends.is_empty() && ends.iter().all(|(_, reason)| reason == "close"),
            "every entered end hook observes its own close boundary: {ends:?}"
        );
        let target_entered = entered.load(Ordering::SeqCst);
        assert!(
            released[..target_entered]
                .iter()
                .all(|flag| !flag.load(Ordering::SeqCst)),
            "no end hook completed while its gate was held"
        );

        drop(senders);
        wait_for("the end hooks held at the deadline to be cancelled", || {
            cancelled.load(Ordering::SeqCst) >= target_entered
        })
        .await;
        assert_eq!(
            cancelled.load(Ordering::SeqCst),
            target_entered,
            "every end hook already entered at the deadline was cancelled"
        );
        wait_for("every subsequently entered end hook to settle", || {
            let entered_count = entered.load(Ordering::SeqCst);
            let completed = released[..entered_count]
                .iter()
                .filter(|flag| flag.load(Ordering::SeqCst))
                .count();
            completed + cancelled.load(Ordering::SeqCst) == entered_count
        })
        .await;
    }

    /// Three unblocked sessions: every close finishes inside the shared
    /// budget, each session boundary is observed exactly once, the table
    /// drains, and no tracked work survives.
    #[tokio::test(start_paused = true)]
    async fn unblocked_sessions_all_finish_inside_the_one_deadline() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicUsize::new(0));
        let released = release_flags(3);
        let (senders, gates) = gate_channels(3);
        drop(senders);
        let rig = shutdown_rig(
            gated_end_extension(
                Arc::clone(&log),
                gates,
                Arc::clone(&entered),
                Arc::clone(&cancelled),
                released.clone(),
                Duration::ZERO,
            ),
            3,
        )
        .await;

        let grace = Duration::from_millis(500);
        let began = Instant::now();
        let report = rig.host.clone().shutdown_after_quiet_wait(grace).await;
        let elapsed = began.elapsed();

        assert!(
            elapsed < grace,
            "no close needed the shared deadline: {elapsed:?}"
        );
        assert_eq!(
            report.sessions_closed, 3,
            "every open session closed: {report:?}"
        );
        assert_eq!(
            report.tasks_remaining, 0,
            "no tracked work survives shutdown: {report:?}"
        );
        let ends = recorded_ends(&log);
        assert_eq!(ends.len(), 3, "one end boundary per session: {ends:?}");
        assert!(
            ends.iter().all(|(_, reason)| reason == "close"),
            "shutdown closes, never other reasons: {ends:?}"
        );
        let unique: HashSet<SessionId> = ends.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            unique.len(),
            3,
            "each session observed its own boundary once: {ends:?}"
        );
        wait_for("every end hook to complete", || {
            released.iter().all(|flag| flag.load(Ordering::SeqCst))
        })
        .await;
        assert_eq!(
            cancelled.load(Ordering::SeqCst),
            0,
            "unblocked end hooks complete instead of being cancelled"
        );
        assert!(rig.host.table_ids().is_empty(), "the session table drained");
    }

    /// Two sessions whose end hooks each hold three quarters of the budget
    /// in controlled time: the first close finishes and publishes inside the
    /// one shared deadline, the second is cut at that same deadline — the
    /// budget is spent across closes, never restarted per session.
    #[tokio::test(start_paused = true)]
    async fn the_close_budget_is_spent_across_closes_not_restarted_per_session() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicUsize::new(0));
        let released = release_flags(2);
        let (senders, gates) = gate_channels(2);
        drop(senders);
        let grace = Duration::from_millis(200);
        let hold = grace * 3 / 4;
        let rig = shutdown_rig(
            gated_end_extension(
                Arc::clone(&log),
                gates,
                Arc::clone(&entered),
                Arc::clone(&cancelled),
                released.clone(),
                hold,
            ),
            2,
        )
        .await;
        let mut subscription = rig.host.subscribe();

        let began = Instant::now();
        let report = rig.host.clone().shutdown_after_quiet_wait(grace).await;
        let elapsed = began.elapsed();

        assert!(
            elapsed >= grace,
            "the second close runs to the shared deadline: {elapsed:?}"
        );
        assert!(
            elapsed < grace * 3 / 2,
            "one shared budget was spent across both closes; a fresh \
             per-session budget would cost two holds: {elapsed:?}"
        );
        assert!(
            report.sessions_closed >= 1,
            "the close that finished inside the budget closed: {report:?}"
        );
        let mut removed = Vec::new();
        while let Ok(Some(HostUpdate::SessionRemoved { session })) =
            tokio::time::timeout(Duration::ZERO, subscription.next()).await
        {
            removed.push(session);
        }
        assert_eq!(
            removed.len(),
            1,
            "exactly the close that finished inside the budget published its removal: {removed:?}"
        );
        let ends = recorded_ends(&log);
        assert!(
            ends.iter().any(|(id, _)| removed.contains(id)),
            "the published removal is one of the closed sessions: {ends:?} vs {removed:?}"
        );
        wait_for("every entered end hook to complete or be cancelled", || {
            let entered_count = entered.load(Ordering::SeqCst);
            let completed = released[..entered_count]
                .iter()
                .filter(|flag| flag.load(Ordering::SeqCst))
                .count();
            completed + cancelled.load(Ordering::SeqCst) == entered_count
        })
        .await;
        let entered_count = entered.load(Ordering::SeqCst);
        let completed = released[..entered_count]
            .iter()
            .filter(|flag| flag.load(Ordering::SeqCst))
            .count();
        assert_eq!(entered_count, 2, "both close hooks entered");
        assert_eq!(completed, 1, "the first released end hook completed");
        assert_eq!(
            cancelled.load(Ordering::SeqCst),
            entered_count - completed,
            "the end hook cut off at the shared deadline was cancelled"
        );
    }
}
