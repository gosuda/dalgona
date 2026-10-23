//! Capability grants: minting, coalescing, persistence, revocation.
//!
//! A `Grant` is minted only here. Request types stay distinct from grant
//! proof: `ensure` answers with a `Grant`, never with broker internals.
//! Persistent approvals live in `<data>/grants.toml` (mode `0600`) via the
//! store's atomic writer; session approvals stay in memory only.
//!
//! `list` returns an owned snapshot so its rows stay valid after the
//! store lock is released.
//!
//! The future that opens a question owns its reservation: dropping it settles
//! the key as cancelled, so a later caller asks again instead of waiting on a
//! question nobody awaits. Persistent writes rebase on the file, never on the
//! memory copy, so an approval cannot restore a row that was revoked while it
//! was pending.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use dal_core::{
    Answer, ClientId, DenyReason, Name, Origin, Question, Service, ServiceSet, Timestamp,
};
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::Broker;
use crate::broker::Settled;
use crate::error::ServiceError;

use super::Caller;
use super::services::ServiceFuture;

mod persist;
use persist::{load, persist as persist_rows};

#[cfg(test)]
mod tests;

/// A minted capability proof. Constructible only in this module.
#[derive(Debug)]
pub(crate) struct Grant {
    key: GrantKey,
    persistent: bool,
}

impl Grant {
    pub(crate) fn key(&self) -> &GrantKey {
        &self.key
    }

    pub(crate) fn persistent(&self) -> bool {
        self.persistent
    }
}

/// Immutable approval identity. Code includes the host-hashed source snapshot,
/// declared version, and exact operation manifest. Workspace is session-scoped;
/// changed code, declaration, or root cannot reuse prior authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrantKey {
    /// The owning extension name.
    pub extension: Name,
    /// The class that registered the extension.
    pub origin: Origin,
    /// The exact granted capability set.
    pub services: ServiceSet,
    /// Host-computed code, version, and declared-operation fingerprint.
    pub digest: Box<str>,
    /// Exact absolute workspace path covered by the approval.
    pub workspace: PathBuf,
}

impl GrantKey {
    /// Constructs the authorization identity of a loaded extension at a root.
    /// An unverified code identity or non-UTF-8/relative root has no grant.
    #[must_use]
    pub fn for_extension(extension: &super::Extension, workspace: &Path) -> Option<Self> {
        if !matches!(extension.origin(), Origin::User | Origin::Bundled)
            || !workspace.is_absolute()
            || workspace.to_str().is_none()
        {
            return None;
        }
        Some(Self {
            extension: extension.name().parse().ok()?,
            origin: extension.origin(),
            services: extension.inject().capabilities(),
            digest: extension.grant_digest()?.into(),
            workspace: workspace.to_path_buf(),
        })
    }

    /// Reports whether this key grants `service`.
    #[must_use]
    pub fn allows(&self, service: Service) -> bool {
        self.services.contains(service)
    }
}

impl Ord for GrantKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.extension
            .cmp(&other.extension)
            .then_with(|| origin_rank(self.origin).cmp(&origin_rank(other.origin)))
            .then_with(|| self.services.cmp(&other.services))
            .then_with(|| self.digest.cmp(&other.digest))
            .then_with(|| self.workspace.cmp(&other.workspace))
    }
}

impl PartialOrd for GrantKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn grant_origin(origin: Origin) -> &'static str {
    match origin {
        Origin::Bundled => "bundled",
        Origin::User => "user",
        Origin::Builtin => "builtin",
        _ => "unknown",
    }
}

fn origin_rank(origin: Origin) -> u8 {
    match origin {
        Origin::Builtin => 0,
        Origin::Bundled => 1,
        Origin::User => 2,
        _ => 3,
    }
}

/// Lifecycle of one grant key.
pub(crate) enum GrantState {
    Absent,
    Asked,
    GrantedPersistent,
    GrantedSession,
}

/// One persisted or session approval row.
#[derive(Clone, Debug)]
pub(crate) struct GrantRow {
    pub ext: Name,
    pub origin: Origin,
    pub services: ServiceSet,
    pub digest: Box<str>,
    pub workspace: PathBuf,
    pub mcp_set: Option<Box<str>>,
    pub by: ClientId,
    pub approved_at: Timestamp,
}

/// One persistent approval row for administration surfaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistentGrant {
    /// The exact granted key.
    pub key: GrantKey,
    /// The canonical declared MCP set digest, if this row is set-specific.
    pub mcp_set: Option<Box<str>>,
    /// The client that approved the grant.
    pub by: ClientId,
    /// When the grant was approved, in UTC.
    pub approved_at: Timestamp,
}

impl PersistentGrant {
    /// Views one stored row as an administration record.
    fn from_row(row: GrantRow) -> Self {
        Self {
            key: GrantKey {
                extension: row.ext,
                origin: row.origin,
                services: row.services,
                digest: row.digest,
                workspace: row.workspace,
            },
            mcp_set: row.mcp_set,
            by: row.by,
            approved_at: row.approved_at,
        }
    }
}

type GrantOutcome = Result<Grant, ServiceError>;

struct Pending {
    done: bool,
    terminal: Option<ServiceError>,
    waiters: usize,
    notify: Arc<Notify>,
}

struct Inner {
    persistent: Vec<GrantRow>,
    session: BTreeMap<GrantKey, GrantRow>,
    pending: BTreeMap<GrantKey, Pending>,
    mcp_session: BTreeMap<McpGrantKey, GrantRow>,
    mcp_pending: BTreeMap<McpGrantKey, Pending>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct McpGrantKey {
    key: GrantKey,
    set: Box<str>,
}

/// Grant store. Session rows die with the store; persistent rows survive
/// through `grants.toml`. One lock guards all maps and is never held across
/// `.await`; sub-millisecond file snapshots run inline, serialized by the
/// persist lock so memory and file cannot diverge.
///
/// Administration handles ([`GrantStore::new`]) share this implementation
/// with the session gate: one file format, one persist path, one error
/// type. Every administration operation re-reads the file, so a running
/// `serve` process observes CLI changes on its next capability check.
pub struct GrantStore {
    data_dir: PathBuf,
    workspace: PathBuf,
    ask_timeout: Duration,
    broker: Option<Arc<Broker>>,
    inner: Mutex<Inner>,
    persist_lock: tokio::sync::Mutex<()>,
    request_update: Mutex<Option<UpdatePublisher>>,
    request_open: Mutex<Option<RequestOpenPublisher>>,
    answerer: Mutex<Option<AnswererProbe>>,
}

/// The update publisher a session installs on its grant store.
pub type UpdatePublisher = Arc<dyn Fn(dal_core::UpdateKind) + Send + Sync>;

/// The actor route a session installs on its grant store: it folds
/// `Event::RequestOpened` for grant questions before they are published,
/// so every settlement journals a record.
pub(crate) type RequestOpenPublisher = Arc<
    dyn Fn(dal_core::Request) -> crate::ext::BoxFuture<'static, Result<(), ServiceError>>
        + Send
        + Sync,
>;

/// The probe a session installs on its grant store to learn whether a
/// front end that can answer extension questions is attached.
pub(crate) type AnswererProbe = Arc<dyn Fn() -> bool + Send + Sync>;

/// Typed grant administration failure.
#[derive(Debug, thiserror::Error)]
pub enum GrantStoreError {
    /// The grants file cannot be read.
    #[error("cannot read grants file: {0}")]
    Read(#[from] std::io::Error),
    /// The grants file, or a grant key bound for it, is malformed; nothing
    /// was written.
    #[error("grants file is malformed: {detail}")]
    Malformed {
        /// What failed to parse.
        detail: Box<str>,
    },
    /// A persistent write failed.
    #[error("cannot persist grants file: {0}")]
    Store(#[from] dal_store::StoreError),
}

/// Ask timeout for administration handles, which never ask.
const ADMIN_ASK_TIMEOUT: Duration = Duration::from_secs(120);

impl GrantStore {
    /// Opens an administration handle over `data_root`. Loading is lazy
    /// so construction never fails: every operation re-reads
    /// `grants.toml` and surfaces [`GrantStoreError`] itself, and no
    /// operation writes when the file cannot be loaded.
    #[must_use]
    pub fn new(data_root: PathBuf) -> Self {
        Self::empty(data_root.clone(), data_root, ADMIN_ASK_TIMEOUT, None)
    }

    /// Installs the session's status publisher for client-facing requests.
    ///
    /// The publisher forwards `UpdateKind::RequestOpened` to the session's
    /// subscribers when a grant request opens; without it, a gate waiting on
    /// the subscription never sees the request.
    pub(crate) fn set_request_update(&self, publisher: UpdatePublisher) {
        *self
            .request_update
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(publisher);
    }

    /// Installs the actor route for grant questions.
    ///
    /// The route folds `Event::RequestOpened` in the session actor before
    /// the question is published, so every settlement journals a record.
    pub(crate) fn set_request_open(&self, publisher: RequestOpenPublisher) {
        *self
            .request_open
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(publisher);
    }

    /// Installs the probe that reports whether a front end that can answer
    /// extension questions is attached now. Without a probe every question
    /// is assumed answerable and the ask timeout alone fails it closed.
    pub(crate) fn set_answerer(&self, probe: AnswererProbe) {
        *self.answerer.lock().unwrap_or_else(PoisonError::into_inner) = Some(probe);
    }

    fn answerer_attached(&self) -> bool {
        let probe = self
            .answerer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(Arc::clone);
        probe.is_none_or(|probe| probe())
    }

    /// Routes one opened grant question through the session actor before
    /// it is published. A dead session skips the route: the question then
    /// expires fail-closed with no answerer.
    async fn open_request(&self, request: &dal_core::Request) -> Result<(), ServiceError> {
        let publisher = self
            .request_open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(Arc::clone);
        if let Some(publisher) = publisher {
            publisher(request.clone()).await?;
        }
        Ok(())
    }

    fn publish_request_update(&self, update: dal_core::UpdateKind) {
        let publisher = self
            .request_update
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(Arc::clone);
        if let Some(publisher) = publisher {
            publisher(update);
        }
    }

    /// Opens a session's grant store with the exact session workspace scope.
    pub(crate) fn with_runtime_scoped(
        data_dir: PathBuf,
        workspace: PathBuf,
        ask_timeout: Duration,
        broker: Arc<Broker>,
    ) -> Result<Self, GrantStoreError> {
        let persistent = load(&data_dir)?;
        let store = Self::empty(data_dir, workspace, ask_timeout, Some(broker));
        store.lock().persistent = persistent;
        Ok(store)
    }

    fn empty(
        data_dir: PathBuf,
        workspace: PathBuf,
        ask_timeout: Duration,
        broker: Option<Arc<Broker>>,
    ) -> Self {
        Self {
            data_dir,
            workspace,
            ask_timeout,
            broker,
            request_update: Mutex::new(None),
            request_open: Mutex::new(None),
            answerer: Mutex::new(None),
            persist_lock: tokio::sync::Mutex::new(()),
            inner: Mutex::new(Inner {
                persistent: Vec::new(),
                session: BTreeMap::new(),
                pending: BTreeMap::new(),
                mcp_session: BTreeMap::new(),
                mcp_pending: BTreeMap::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn ensure<'a>(
        &'a self,
        who: &'a Caller,
        service: Service,
        cancel: &'a CancellationToken,
    ) -> ServiceFuture<'a, Grant> {
        Box::pin(async move { self.ensure_inner(who, service, cancel).await })
    }

    /// Ensures a grant for one plugin's exact declared MCP set.
    pub(crate) fn ensure_declared_mcp<'a>(
        &'a self,
        who: &'a Caller,
        set: Box<str>,
        detail: Box<str>,
        cancel: &'a CancellationToken,
    ) -> ServiceFuture<'a, Grant> {
        Box::pin(async move {
            self.ensure_declared_mcp_inner(who, set, detail, cancel)
                .await
        })
    }

    /// Denies one MCP reservation without prompting: headless runs,
    /// cancellation, and the ask timeout all settle the same way.
    async fn deny_mcp(&self, key: &McpGrantKey, reservation: &Arc<Notify>) -> GrantOutcome {
        self.finalize_mcp(
            key,
            Err(ServiceError::Denied(DenyReason::NotGranted)),
            None,
            reservation,
        )
        .await
    }

    /// Only the host's captured caller identity may authorize an extension.
    fn key_for(&self, who: &Caller) -> Result<GrantKey, ServiceError> {
        let digest = who.grant_digest.clone().unwrap_or_default();
        if who.origin != Origin::Builtin
            && (digest.len() != 64
                || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !self.workspace.is_absolute()
                || self.workspace.to_str().is_none())
        {
            return Err(ServiceError::Denied(DenyReason::NotGranted));
        }
        Ok(GrantKey {
            extension: who.ext.clone(),
            origin: who.origin,
            services: who.inject.capabilities(),
            digest,
            workspace: self.workspace.clone(),
        })
    }

    async fn ensure_declared_mcp_inner(
        &self,
        who: &Caller,
        set: Box<str>,
        detail: Box<str>,
        cancel: &CancellationToken,
    ) -> GrantOutcome {
        let base = self.key_for(who)?;
        if who.origin == Origin::Builtin {
            return Ok(Grant {
                key: base,
                persistent: false,
            });
        }
        let Some(turn) = who.turn else {
            return Err(ServiceError::Denied(DenyReason::NotGranted));
        };
        self.refresh_persistent()
            .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        let key = McpGrantKey { key: base, set };
        let notify = match self.mcp_slot(&key) {
            (GrantState::GrantedPersistent, _) => {
                return Ok(Grant {
                    key: key.key,
                    persistent: true,
                });
            }
            (GrantState::GrantedSession, _) => {
                return Ok(Grant {
                    key: key.key,
                    persistent: false,
                });
            }
            (GrantState::Asked, Some(notify)) => {
                return self.collect_mcp(&key, notify, cancel).await;
            }
            (GrantState::Absent, Some(notify)) => notify,
            _ => return Err(ServiceError::Cancelled),
        };
        // Abandoning this future settles the reservation as cancelled: a later
        // caller asks again instead of waiting on a question nobody awaits.
        let _abandon = McpCommitGuard {
            store: self,
            key: key.clone(),
            reservation: Arc::clone(&notify),
        };
        if !self.answerer_attached() {
            return self.deny_mcp(&key, &notify).await;
        }
        let capabilities = key
            .key
            .services
            .iter()
            .map(|service| service.as_str().into())
            .collect();
        let origin = grant_origin(who.origin);
        let question = Question::Grant {
            ext: who.ext.as_str().into(),
            origin: origin.into(),
            capabilities,
            detail: Some(format!("{detail}\n{}", grant_scope_text(&key.key)).into()),
        };
        let owner = dal_core::Owner::Extension {
            name: who.ext.as_str().into(),
            origin: origin.into(),
        };
        let deadline = Instant::now() + self.ask_timeout;
        let Some(broker) = self.broker.as_ref() else {
            return Err(ServiceError::failed(None, "grant store has no broker"));
        };
        let (request, answer) = broker.open(owner, question, turn, deadline);
        if let Err(error) = self.open_request(&request).await {
            if let Ok(resolved) = broker.answer(request.id, Answer::Cancel, ClientId::new("core")) {
                broker.cancel(&resolved);
            }
            return self.finalize_mcp(&key, Err(error), None, &notify).await;
        }
        self.publish_request_update(dal_core::UpdateKind::RequestOpened(request.clone()));
        tokio::select! {
            biased;
            () = cancel.cancelled() => self.deny_mcp(&key, &notify).await,
            () = tokio::time::sleep(self.ask_timeout) => self.deny_mcp(&key, &notify).await,
            () = notify.notified() => self.take_mcp(&key),
            Settled { answer, by, .. } = answer => {
                let (outcome, mut row) = Self::decide(&key.key, &answer, by.clone());
                if let Some(row) = &mut row {
                    row.mcp_set = Some(key.set.clone());
                }
                self.finalize_mcp(&key, outcome, row, &notify).await
            }
        }
    }

    async fn ensure_inner(
        &self,
        who: &Caller,
        service: Service,
        cancel: &CancellationToken,
    ) -> Result<Grant, ServiceError> {
        if !who.inject.contains(service) {
            return Err(ServiceError::Denied(DenyReason::NotInjected));
        }
        let key = self.key_for(who)?;
        if who.origin == Origin::Builtin {
            return Ok(Grant {
                key,
                persistent: false,
            });
        }
        // A cell's approval is its grant; a cell that was never approved has none.
        if service == Service::Ask || who.cell_approved() {
            return Ok(Grant {
                key,
                persistent: false,
            });
        }
        // Reload persistent rows before every probe so a CLI revocation
        // lands on the next capability check. Malformed content fails
        // closed here; the file is never written on this path.
        self.refresh_persistent()
            .map_err(|e| ServiceError::failed(None, e.to_string()))?;
        let notify = match self.slot(&key) {
            (GrantState::GrantedPersistent, _) => {
                return Ok(Grant {
                    key,
                    persistent: true,
                });
            }
            (GrantState::GrantedSession, _) => {
                return Ok(Grant {
                    key,
                    persistent: false,
                });
            }
            (GrantState::Asked, Some(notify)) => {
                return self.collect(&key, notify, cancel).await;
            }
            (GrantState::Absent, Some(notify)) => notify,
            _ => return Err(ServiceError::Cancelled),
        };
        // Abandoning this future settles the reservation as cancelled: a later
        // caller asks again instead of waiting on a question nobody awaits.
        let _abandon = CommitGuard {
            store: self,
            key: key.clone(),
            reservation: Arc::clone(&notify),
        };
        // A question needs a front end that can answer it. With none
        // attached (print mode, listen-only clients) nobody can approve, so
        // the reservation resolves as denied now instead of holding the
        // caller for the whole ask timeout. A caller without a turn (a slash
        // command) asks like any other: its request just has no turn to end
        // with, so only an answer, the deadline, or session cancel settles it.
        if !self.answerer_attached() {
            return self
                .finalize(
                    &key,
                    service,
                    Err(ServiceError::Denied(DenyReason::NotGranted)),
                    None,
                    &notify,
                )
                .await;
        }
        let capabilities: Vec<Box<str>> = key.services.iter().map(|s| s.as_str().into()).collect();
        let origin = match who.origin {
            Origin::Bundled => "bundled",
            Origin::User => "user",
            Origin::Builtin => "builtin",
            _ => "unknown",
        };
        let question = Question::Grant {
            ext: who.ext.as_str().into(),
            origin: origin.into(),
            capabilities,
            detail: Some(grant_scope_text(&key).into()),
        };
        let owner = dal_core::Owner::Extension {
            name: who.ext.as_str().into(),
            origin: origin.into(),
        };
        let deadline = Instant::now() + self.ask_timeout;
        let Some(broker) = self.broker.as_ref() else {
            return Err(ServiceError::failed(None, "grant store has no broker"));
        };
        let (request, answer) = broker.open(owner, question, who.turn, deadline);
        if let Err(error) = self.open_request(&request).await {
            let outcome = Err(error);
            if let Ok(resolved) = broker.answer(request.id, Answer::Cancel, ClientId::new("core")) {
                broker.cancel(&resolved);
            }
            return self.finalize(&key, service, outcome, None, &notify).await;
        }
        self.publish_request_update(dal_core::UpdateKind::RequestOpened(request.clone()));
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                self.finalize(&key, service, Err(ServiceError::Denied(DenyReason::NotGranted)), None, &notify).await
            }
            () = tokio::time::sleep(self.ask_timeout) => {
                self.finalize(&key, service, Err(ServiceError::Denied(DenyReason::NotGranted)), None, &notify).await
            }
            () = notify.notified() => self.take(&key),
            Settled { answer, by, .. } = answer => {
                let (outcome, row) = GrantStore::decide(&key, &answer, by.clone());
                self.finalize(&key, service, outcome, row, &notify).await
            }
        }
    }

    fn mcp_slot(&self, key: &McpGrantKey) -> (GrantState, Option<Arc<Notify>>) {
        let mut inner = self.lock();
        if inner.persistent.iter().any(|row| mcp_row_matches(row, key)) {
            return (GrantState::GrantedPersistent, None);
        }
        if inner.mcp_session.contains_key(key) {
            return (GrantState::GrantedSession, None);
        }
        if let Some(pending) = inner.mcp_pending.get_mut(key) {
            pending.waiters += 1;
            return (GrantState::Asked, Some(Arc::clone(&pending.notify)));
        }
        let notify = Arc::new(Notify::new());
        inner.mcp_pending.insert(
            key.clone(),
            Pending {
                done: false,
                terminal: None,
                waiters: 0,
                notify: Arc::clone(&notify),
            },
        );
        (GrantState::Absent, Some(notify))
    }

    async fn collect_mcp(
        &self,
        key: &McpGrantKey,
        notify: Arc<Notify>,
        cancel: &CancellationToken,
    ) -> GrantOutcome {
        let mut waiter = WaiterGuard {
            store: self,
            waiting: Waiting::Mcp(key),
            armed: true,
        };
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(outcome) = self.mcp_take_if_done(key) {
            waiter.armed = false;
            return outcome;
        }
        tokio::select! {
            biased;
            // The armed guard releases the registration.
            () = cancel.cancelled() => Err(ServiceError::Denied(DenyReason::NotGranted)),
            () = &mut notified => {
                waiter.armed = false;
                self.take_mcp(key)
            }
        }
    }

    fn take_mcp(&self, key: &McpGrantKey) -> GrantOutcome {
        self.mcp_take_if_done(key)
            .unwrap_or(Err(ServiceError::Cancelled))
    }

    fn mcp_take_if_done(&self, key: &McpGrantKey) -> Option<GrantOutcome> {
        let mut inner = self.lock();
        if inner.persistent.iter().any(|row| mcp_row_matches(row, key)) {
            Self::mcp_release(&mut inner, key);
            return Some(Ok(Grant {
                key: key.key.clone(),
                persistent: true,
            }));
        }
        if inner.mcp_session.contains_key(key) {
            Self::mcp_release(&mut inner, key);
            return Some(Ok(Grant {
                key: key.key.clone(),
                persistent: false,
            }));
        }
        let terminal = match inner.mcp_pending.get(key) {
            Some(pending) if pending.done => pending.terminal.clone(),
            Some(_) => return None,
            None => return Some(Err(ServiceError::Cancelled)),
        };
        Self::mcp_release(&mut inner, key);
        Some(Err(terminal.unwrap_or(ServiceError::Cancelled)))
    }

    fn mcp_abandon(&self, key: &McpGrantKey) {
        Self::mcp_release(&mut self.lock(), key);
    }

    fn mcp_release(inner: &mut Inner, key: &McpGrantKey) {
        if let Some(pending) = inner.mcp_pending.get_mut(key) {
            pending.waiters = pending.waiters.saturating_sub(1);
            if pending.waiters == 0 && pending.done {
                inner.mcp_pending.remove(key);
            }
        }
    }

    /// Single-lock slot probe: granted, waiting, or freshly reserved.
    /// Waiters attach under the same lock that reserves, so a waiter can
    /// never miss the terminal notification.
    fn slot(&self, key: &GrantKey) -> (GrantState, Option<Arc<Notify>>) {
        let mut inner = self.lock();
        if inner
            .persistent
            .iter()
            .any(|row| row.mcp_set.is_none() && row_active(row) && row_key(row) == *key)
        {
            return (GrantState::GrantedPersistent, None);
        }
        if inner.session.contains_key(key) {
            return (GrantState::GrantedSession, None);
        }
        if let Some(pending) = inner.pending.get_mut(key) {
            pending.waiters += 1;
            return (GrantState::Asked, Some(pending.notify.clone()));
        }
        let notify = Arc::new(Notify::new());
        inner.pending.insert(
            key.clone(),
            Pending {
                done: false,
                terminal: None,
                waiters: 0,
                notify: notify.clone(),
            },
        );
        (GrantState::Absent, Some(notify))
    }

    async fn collect(
        &self,
        key: &GrantKey,
        notify: Arc<Notify>,
        cancel: &CancellationToken,
    ) -> GrantOutcome {
        let mut waiter = WaiterGuard {
            store: self,
            waiting: Waiting::Plain(key),
            armed: true,
        };
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(outcome) = self.take_if_done(key) {
            waiter.armed = false;
            return outcome;
        }
        tokio::select! {
            biased;
            // The armed guard releases the registration.
            () = cancel.cancelled() => Err(ServiceError::Denied(DenyReason::NotGranted)),
            () = &mut notified => {
                waiter.armed = false;
                self.take(key)
            }
        }
    }

    /// Resolves one woken waiter: stored rows mint a fresh proof, a stored
    /// terminal replays, a missing entry means revocation. Returns `None`
    /// while the key is still pending.
    fn take_if_done(&self, key: &GrantKey) -> Option<GrantOutcome> {
        let mut inner = self.lock();
        if inner
            .persistent
            .iter()
            .any(|row| row.mcp_set.is_none() && row_active(row) && row_key(row) == *key)
        {
            Self::release(&mut inner, key);
            return Some(Ok(Grant {
                key: key.clone(),
                persistent: true,
            }));
        }
        if inner.session.contains_key(key) {
            Self::release(&mut inner, key);
            return Some(Ok(Grant {
                key: key.clone(),
                persistent: false,
            }));
        }
        let terminal = match inner.pending.get(key) {
            Some(pending) if pending.done => pending.terminal.clone(),
            Some(_) => return None,
            None => return Some(Err(ServiceError::Cancelled)),
        };
        Self::release(&mut inner, key);
        Some(Err(terminal.unwrap_or(ServiceError::Cancelled)))
    }

    fn take(&self, key: &GrantKey) -> GrantOutcome {
        self.take_if_done(key)
            .unwrap_or(Err(ServiceError::Cancelled))
    }

    fn abandon(&self, key: &GrantKey) {
        Self::release(&mut self.lock(), key);
    }

    fn release(inner: &mut Inner, key: &GrantKey) {
        if let Some(pending) = inner.pending.get_mut(key) {
            pending.waiters = pending.waiters.saturating_sub(1);
            if pending.waiters == 0 && pending.done {
                inner.pending.remove(key);
            }
        }
    }

    fn decide(key: &GrantKey, answer: &Answer, by: ClientId) -> (GrantOutcome, Option<GrantRow>) {
        match answer {
            Answer::Approve | Answer::ApproveForSession => {
                let persistent = matches!(answer, Answer::Approve);
                let grant = Grant {
                    key: key.clone(),
                    persistent,
                };
                let row = GrantRow {
                    ext: key.extension.clone(),
                    origin: key.origin,
                    services: key.services,
                    digest: key.digest.clone(),
                    workspace: key.workspace.clone(),
                    mcp_set: None,
                    by,
                    approved_at: Timestamp::now(),
                };
                (Ok(grant), Some(row))
            }
            Answer::Cancel => (Err(ServiceError::Cancelled), None),
            Answer::Decline | Answer::Value(_) | _ => (Err(ServiceError::Declined), None),
        }
    }

    /// Applies one terminal outcome: stores rows, persists, notifies
    /// waiters, and releases the key. A missing or re-reserved entry
    /// (revoked meanwhile) drops the outcome so a late answer can never
    /// restore it.
    async fn finalize(
        &self,
        key: &GrantKey,
        service: Service,
        outcome: GrantOutcome,
        row: Option<GrantRow>,
        reservation: &Arc<Notify>,
    ) -> GrantOutcome {
        // Serialize all file writers: revoke orders its rewrite behind us,
        // so memory and file cannot diverge and no lock order inverts.
        let _writer = self.persist_lock.lock().await;
        let _commit = CommitGuard {
            store: self,
            key: key.clone(),
            reservation: Arc::clone(reservation),
        };
        let mut outcome = outcome;
        let persistent_row = {
            let mut inner = self.lock();
            let valid = match inner.pending.get_mut(key) {
                Some(pending) => !pending.done && Arc::ptr_eq(&pending.notify, reservation),
                None => false,
            };
            if !valid {
                return Err(ServiceError::Cancelled);
            }
            match (row, &outcome) {
                (Some(row), Ok(grant)) if grant.persistent() => Some(row),
                (Some(row), Ok(_)) => {
                    inner.session.insert(key.clone(), row);
                    None
                }
                _ => None,
            }
        };
        if let Some(row) = persistent_row
            && let Err(error) = self.append_persistent(row)
        {
            outcome = Err(ServiceError::failed(Some(service), error.to_string()));
        }
        let mut inner = self.lock();
        let matched = match inner.pending.get_mut(key) {
            Some(pending) => Arc::ptr_eq(&pending.notify, reservation),
            None => false,
        };
        if !matched {
            drop(inner);
            // The row was already persisted above; remove it so a later
            // refresh cannot resurrect a cancelled grant.
            if let Err(error) = self.withdraw_persistent(key, None) {
                return Err(ServiceError::failed(Some(service), error.to_string()));
            }
            return Err(ServiceError::Cancelled);
        }
        if let Err(error) = &outcome
            && let Some(pending) = inner.pending.get_mut(key)
        {
            pending.terminal = Some(error.clone());
        }
        if let Some(pending) = inner.pending.get_mut(key) {
            pending.done = true;
            pending.notify.notify_waiters();
            if pending.waiters == 0 {
                inner.pending.remove(key);
            }
        }
        outcome
    }

    async fn finalize_mcp(
        &self,
        key: &McpGrantKey,
        mut outcome: GrantOutcome,
        row: Option<GrantRow>,
        reservation: &Arc<Notify>,
    ) -> GrantOutcome {
        let _writer = self.persist_lock.lock().await;
        let _commit = McpCommitGuard {
            store: self,
            key: key.clone(),
            reservation: Arc::clone(reservation),
        };
        let persistent_row = {
            let mut inner = self.lock();
            let valid = match inner.mcp_pending.get(key) {
                Some(pending) => !pending.done && Arc::ptr_eq(&pending.notify, reservation),
                None => false,
            };
            if !valid {
                return Err(ServiceError::Cancelled);
            }
            match (row, &outcome) {
                (Some(row), Ok(grant)) if grant.persistent() => Some(row),
                (Some(row), Ok(_)) => {
                    inner.mcp_session.insert(key.clone(), row);
                    None
                }
                _ => None,
            }
        };
        if let Some(row) = persistent_row
            && let Err(error) = self.append_persistent(row)
        {
            outcome = Err(ServiceError::failed(Some(Service::Mcp), error.to_string()));
        }
        let mut inner = self.lock();
        let matched = match inner.mcp_pending.get_mut(key) {
            Some(pending) => Arc::ptr_eq(&pending.notify, reservation),
            None => false,
        };
        if !matched {
            drop(inner);
            // The row was already persisted above; remove it so a later
            // refresh cannot resurrect a cancelled grant.
            if let Err(error) = self.withdraw_persistent(&key.key, Some(key.set.as_ref())) {
                return Err(ServiceError::failed(Some(Service::Mcp), error.to_string()));
            }
            return Err(ServiceError::Cancelled);
        }
        if let Err(error) = &outcome
            && let Some(pending) = inner.mcp_pending.get_mut(key)
        {
            pending.terminal = Some(error.clone());
        }
        if let Some(pending) = inner.mcp_pending.get_mut(key) {
            pending.done = true;
            pending.notify.notify_waiters();
            if pending.waiters == 0 {
                inner.mcp_pending.remove(key);
            }
        }
        outcome
    }

    /// Adds `row` to the approvals stored on disk. Callers hold
    /// `persist_lock`.
    ///
    /// The file is the base, never the memory copy: that copy can predate a
    /// revocation made elsewhere while this approval was pending, and writing
    /// it back would resurrect the revoked rows. An identical active row is
    /// kept once.
    fn append_persistent(&self, row: GrantRow) -> Result<(), GrantStoreError> {
        let mut rows = load(&self.data_dir)?;
        let wanted = row_key(&row);
        let stored = rows
            .iter()
            .any(|held| held.mcp_set == row.mcp_set && row_active(held) && row_key(held) == wanted);
        if !stored {
            rows.push(row);
        }
        persist_rows(&self.data_dir, &rows)?;
        self.lock().persistent = rows;
        Ok(())
    }

    /// Removes the rows an approval stored for `key` and `mcp_set`. Callers
    /// hold `persist_lock`; the file is the base, as in
    /// [`Self::append_persistent`].
    fn withdraw_persistent(
        &self,
        key: &GrantKey,
        mcp_set: Option<&str>,
    ) -> Result<(), GrantStoreError> {
        let mut rows = load(&self.data_dir)?;
        rows.retain(|held| held.mcp_set.as_deref() != mcp_set || row_key(held) != *key);
        persist_rows(&self.data_dir, &rows)?;
        self.lock().persistent = rows;
        Ok(())
    }

    /// Reloads persistent rows from disk, replacing memory. Fails closed
    /// on malformed content; the file is never written here. Every
    /// capability check and administration operation runs this first, so
    /// a CLI change lands on the next check in any process.
    fn refresh_persistent(&self) -> Result<(), GrantStoreError> {
        let rows = load(&self.data_dir)?;
        self.lock().persistent = rows;
        Ok(())
    }

    /// Lists every persistent grant, sorted by extension name bytes, then
    /// origin, then service set. Re-reads the file; a malformed file
    /// fails without touching it.
    ///
    /// # Errors
    /// Returns [`GrantStoreError`] when the grants file cannot be read.
    pub async fn list(&self) -> Result<Vec<PersistentGrant>, GrantStoreError> {
        let _writer = self.persist_lock.lock().await;
        let rows = load(&self.data_dir)?;
        self.lock().persistent.clone_from(&rows);
        let mut out: Vec<PersistentGrant> =
            rows.into_iter().map(PersistentGrant::from_row).collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    /// Reports whether the exact key is persistently granted. Re-reads
    /// the file; a malformed file fails without touching it.
    ///
    /// # Errors
    /// Returns [`GrantStoreError`] when the grants file cannot be read.
    pub async fn contains(&self, key: &GrantKey) -> Result<bool, GrantStoreError> {
        let _writer = self.persist_lock.lock().await;
        let rows = load(&self.data_dir)?;
        self.lock().persistent.clone_from(&rows);
        Ok(rows
            .iter()
            .any(|row| row.mcp_set.is_none() && row_active(row) && row_key(row) == *key))
    }

    /// Persists exactly `key`, attributed to `by` at `at`. Returns `false`
    /// without rewriting the file when the exact key already exists, so
    /// granting is idempotent. A changed service set is a new key.
    ///
    /// # Errors
    /// Returns [`GrantStoreError::Malformed`] naming the cause for a key
    /// that could not authorize a call or load back, and other
    /// [`GrantStoreError`]s when the grants file cannot be read or written.
    pub async fn grant(
        &self,
        key: GrantKey,
        by: ClientId,
        at: Timestamp,
    ) -> Result<bool, GrantStoreError> {
        // A key that cannot authorize a call, or whose row no later load
        // accepts, is rejected here so one bad call cannot brick the file.
        check_storable(&key)?;
        let _writer = self.persist_lock.lock().await;
        let mut rows = load(&self.data_dir)?;
        if rows
            .iter()
            .any(|row| row.mcp_set.is_none() && row_active(row) && row_key(row) == key)
        {
            self.lock().persistent = rows;
            return Ok(false);
        }
        rows.push(GrantRow {
            ext: key.extension.clone(),
            origin: key.origin,
            services: key.services,
            digest: key.digest.clone(),
            workspace: key.workspace.clone(),
            mcp_set: None,
            by,
            approved_at: at,
        });
        persist_rows(&self.data_dir, &rows)?;
        self.lock().persistent = rows;
        Ok(true)
    }

    /// Removes every persistent grant for `extension`, for `User` and
    /// `Bundled` origins alike, and returns the removed count. Idempotent:
    /// nothing matches, nothing rewrites, count zero. In-flight waiters
    /// for the extension resolve cancelled, mirroring the session path.
    /// Session grants are untouched and remain until their sessions end.
    ///
    /// # Errors
    /// Returns [`GrantStoreError`] when the grants file cannot be read or
    /// written.
    pub async fn revoke(&self, extension: &Name) -> Result<usize, GrantStoreError> {
        let _writer = self.persist_lock.lock().await;
        let rows = load(&self.data_dir)?;
        let before = rows.len();
        let kept: Vec<GrantRow> = rows
            .into_iter()
            .filter(|row| row.ext != *extension)
            .collect();
        let removed = before - kept.len();
        if removed > 0 {
            persist_rows(&self.data_dir, &kept)?;
        }
        let woken = {
            let mut inner = self.lock();
            inner.persistent = kept;
            Self::drop_pending(&mut inner, extension)
        };
        for notify in woken {
            notify.notify_waiters();
        }
        Ok(removed)
    }

    /// Removes every in-flight question of `extension`, plain and MCP alike,
    /// and returns the waiters' wakers. A removed entry can never settle, so a
    /// late answer cannot restore a revoked approval; each woken waiter finds
    /// no entry and resolves cancelled.
    fn drop_pending(inner: &mut Inner, extension: &Name) -> Vec<Arc<Notify>> {
        let mut woken = Vec::new();
        inner.pending.retain(|key, pending| {
            let revoked = key.extension == *extension;
            if revoked {
                woken.push(Arc::clone(&pending.notify));
            }
            !revoked
        });
        inner.mcp_pending.retain(|key, pending| {
            let revoked = key.key.extension == *extension;
            if revoked {
                woken.push(Arc::clone(&pending.notify));
            }
            !revoked
        });
        woken
    }
}

/// Checks that `key` could authorize a call and that its row loads back.
fn check_storable(key: &GrantKey) -> Result<(), GrantStoreError> {
    let reject = |detail: &str| GrantStoreError::Malformed {
        detail: format!("grant key rejected, nothing was written: {detail}").into(),
    };
    if key.services.is_empty() {
        return Err(reject(
            "it grants no service; add at least one service other than ask",
        ));
    }
    if !matches!(key.origin, Origin::User | Origin::Bundled) {
        return Err(reject(
            "only user and bundled extensions hold grants; builtin extensions need none",
        ));
    }
    if key.digest.len() != 64 || !key.digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(reject(
            "the code digest must be 64 hexadecimal characters; build the key from a loaded extension",
        ));
    }
    if !key.workspace.is_absolute() || key.workspace.to_str().is_none() {
        return Err(reject("the workspace must be an absolute UTF-8 path"));
    }
    Ok(())
}

fn mcp_row_matches(row: &GrantRow, key: &McpGrantKey) -> bool {
    row_active(row) && row.mcp_set.as_deref() == Some(key.set.as_ref()) && row_key(row) == key.key
}

/// Durable approvals expire after 90 days even when code and root do not change.
fn row_active(row: &GrantRow) -> bool {
    let now = Timestamp::now().as_second();
    let approved = row.approved_at.as_second();
    approved <= now
        && approved
            .checked_add(90 * 24 * 60 * 60)
            .is_some_and(|expiry| now < expiry)
}

fn grant_scope_text(key: &GrantKey) -> String {
    format!(
        "Code identity: {}\nWorkspace: {}\nPersistent approval expires after 90 days.",
        key.digest,
        key.workspace.display()
    )
}

fn row_key(row: &GrantRow) -> GrantKey {
    GrantKey {
        extension: row.ext.clone(),
        origin: row.origin,
        services: row.services,
        digest: row.digest.clone(),
        workspace: row.workspace.clone(),
    }
}

/// Fail-closed owner of one reservation. The opener arms it the moment it
/// reserves the key and `finalize` arms a second one around the commit: if
/// the opener future is dropped at any point before a terminal outcome,
/// waiters must not hang on a `Notify` that never fires and later callers
/// must not queue behind a question nobody awaits. The guard resolves the
/// entry as cancelled and releases it; the normal path sets `done` first,
/// which neutralizes the guard without disarming. Rows stay memory-only on
/// this path and converge on the next persist; a crash loses them, which
/// fails closed toward re-approval.
struct CommitGuard<'a> {
    store: &'a GrantStore,
    key: GrantKey,
    reservation: Arc<Notify>,
}

/// The [`CommitGuard`] of a declared-MCP reservation.
struct McpCommitGuard<'a> {
    store: &'a GrantStore,
    key: McpGrantKey,
    reservation: Arc<Notify>,
}

/// The pending entry one waiter registered on.
#[derive(Clone, Copy)]
enum Waiting<'a> {
    Plain(&'a GrantKey),
    Mcp(&'a McpGrantKey),
}

/// Releases a waiter's registration when its future is dropped before a
/// terminal outcome released it. A stranded count would keep a settled entry
/// alive and replay its stale terminal to every later caller instead of
/// asking again.
struct WaiterGuard<'a> {
    store: &'a GrantStore,
    waiting: Waiting<'a>,
    armed: bool,
}

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match self.waiting {
            Waiting::Plain(key) => self.store.abandon(key),
            Waiting::Mcp(key) => self.store.mcp_abandon(key),
        }
    }
}

impl Drop for McpCommitGuard<'_> {
    fn drop(&mut self) {
        let mut inner = self.store.lock();
        let notify = match inner.mcp_pending.get_mut(&self.key) {
            Some(pending) if !pending.done && Arc::ptr_eq(&pending.notify, &self.reservation) => {
                pending.done = true;
                pending.terminal = Some(ServiceError::Cancelled);
                Some(Arc::clone(&pending.notify))
            }
            _ => None,
        };
        let remove = inner
            .mcp_pending
            .get(&self.key)
            .is_some_and(|pending| pending.waiters == 0 && pending.done);
        if remove {
            inner.mcp_pending.remove(&self.key);
        }
        drop(inner);
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
    }
}

impl Drop for CommitGuard<'_> {
    fn drop(&mut self) {
        let mut inner = self.store.lock();
        let notify = match inner.pending.get_mut(&self.key) {
            Some(pending) if !pending.done && Arc::ptr_eq(&pending.notify, &self.reservation) => {
                pending.done = true;
                pending.terminal = Some(ServiceError::Cancelled);
                Some(pending.notify.clone())
            }
            _ => None,
        };
        let Some(notify) = notify else { return };
        inner.persistent.retain(|row| row_key(row) != self.key);
        inner.session.remove(&self.key);
        notify.notify_waiters();
        if let Some(pending) = inner.pending.get_mut(&self.key)
            && pending.waiters == 0
        {
            inner.pending.remove(&self.key);
        }
    }
}
