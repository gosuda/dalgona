//! Host-owned live sessions and their process-independent inputs.

mod history;
pub(crate) mod ops;
mod registry;
mod start;

/// Extension record identity for a child session's durable start policy.
pub(crate) const CHILD_POLICY_EXT: &str = "dal-agent";
/// Extension record kind for a child session's durable start policy.
pub(crate) const CHILD_POLICY_KIND: &str = "child_policy";

pub use ops::DocEntry;

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dal_core::{CallId, Config, SessionId, Stop, Workspace};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::admission::Admission;
use crate::ext::generation::Generation;
use crate::session::backend::Backend;

/// Captured process inputs passed into the host; no operation reads ambient process state.
#[derive(Clone, Debug)]
pub struct Env {
    /// The captured environment variables.
    pub vars: BTreeMap<OsString, OsString>,
    /// The captured working directory.
    pub cwd: PathBuf,
    /// The executable used to run the sandbox helper, when the edge provides one.
    pub sandbox_helper: Option<PathBuf>,
}

impl Env {
    /// Builds an environment with no variables and no sandbox helper whose working directory is `root`.
    #[must_use]
    pub fn data_root(root: PathBuf) -> Self {
        Self {
            vars: BTreeMap::new(),
            cwd: root,
            sandbox_helper: None,
        }
    }
}

/// Selects a durable, ephemeral, or child session in an explicit workspace.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SessionRef {
    /// Create a new durable session.
    New {
        /// The session workspace.
        workspace: Workspace,
        /// An optional display name.
        name: Option<Box<str>>,
    },
    /// Resume a session by its id or unique name.
    Resume {
        /// The id or name to resolve.
        key: Box<str>,
        /// The session workspace.
        workspace: Workspace,
    },
    /// Continue the newest session in the workspace, creating one if needed.
    Continue {
        /// The session workspace.
        workspace: Workspace,
    },
    /// Create an in-memory session without a journal or lock.
    Ephemeral {
        /// The session workspace.
        workspace: Workspace,
    },
    /// Create a child session for one parent tool call.
    Child {
        /// The parent session.
        parent: SessionId,
        /// The call that requested the child.
        call: CallId,
        /// The child workspace.
        workspace: Workspace,
        /// An optional display name for the child session.
        name: Option<Box<str>>,
    },
}

/// A host-level lifecycle update.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum HostUpdate {
    /// A session has been opened.
    SessionAdded {
        /// The opened session.
        session: SessionId,
    },
    /// A session has been closed.
    SessionRemoved {
        /// The closed session.
        session: SessionId,
    },
    /// A session's visible metadata has changed.
    SessionChanged {
        /// The session whose metadata changed.
        session: SessionId,
    },
    /// A child session has started.
    ChildStarted {
        /// The parent session.
        parent: SessionId,
        /// The child session.
        child: SessionId,
        /// The child display name.
        name: Box<str>,
    },
    /// A child session has ended.
    ChildEnded {
        /// The parent session.
        parent: SessionId,
        /// The child session.
        child: SessionId,
        /// The child stop reason.
        stop: Stop,
    },
}

/// The aggregate outcome of orderly host shutdown.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShutdownReport {
    /// The number of open sessions closed by shutdown.
    pub sessions_closed: usize,
    /// Whether all registered extension status kinds went quiet within grace.
    pub status_quiet: bool,
    /// Session-owned tracked tasks still pending after shutdown.
    pub tasks_remaining: usize,
}

impl Default for ShutdownReport {
    fn default() -> Self {
        Self {
            sessions_closed: 0,
            status_quiet: true,
            tasks_remaining: 0,
        }
    }
}

/// A stream of host lifecycle updates. Dropping it unregisters the subscriber.
pub struct HostSubscription {
    pub(crate) receiver: mpsc::UnboundedReceiver<HostUpdate>,
}

impl HostSubscription {
    /// Returns the next host update, or `None` after host shutdown.
    pub async fn next(&mut self) -> Option<HostUpdate> {
        self.receiver.recv().await
    }
}

/// Owns live sessions, their actors, and host-level subscriptions.
#[derive(Clone)]
pub struct Host {
    pub(crate) state: Arc<HostState>,
}

pub(crate) struct HostState {
    pub(crate) sessions: Mutex<HashMap<SessionId, SessionEntry>>,
    /// Names reserved while a new session is being opened.
    pub(crate) name_claims: Mutex<HashMap<(Workspace, Box<str>), SessionId>>,
    pub(crate) subscribers: Mutex<Vec<mpsc::UnboundedSender<HostUpdate>>>,
    pub(crate) shared: Arc<HostShared>,
    /// The futures of the extensions' `Attach` controllers; shutdown aborts them.
    pub(crate) attached: Mutex<tokio::task::JoinSet<()>>,
}

pub(crate) struct NameClaim {
    pub(crate) state: Arc<HostState>,
    pub(crate) key: (Workspace, Box<str>),
    pub(crate) id: SessionId,
}

impl Drop for NameClaim {
    fn drop(&mut self) {
        let mut claims = self
            .state
            .name_claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if claims.get(&self.key).is_some_and(|id| *id == self.id) {
            claims.remove(&self.key);
        }
    }
}

/// Product, configuration, and runtime handles shared by every session.
pub(crate) struct HostShared {
    /// The validated product configuration.
    pub(crate) config: Config,
    /// The captured process inputs.
    pub(crate) env: Arc<Env>,
    /// The product name.
    pub(crate) product_name: &'static str,
    /// The owned data root.
    pub(crate) data_root: PathBuf,
    /// The bounded admission gates.
    pub(crate) admission: Admission,
    /// The host-wide interpreter worker pool (R09).
    pub(crate) interpreters: Arc<crate::admission::Interpreters>,
    /// The host's providers.
    pub(crate) providers: dal_provider::ProviderSet,
    /// The merged command table.
    pub(crate) commands: Arc<[dal_core::CommandSpec]>,
    /// The current extension generation.
    pub(crate) generation: watch::Sender<Arc<Generation>>,
    /// The latest fetched provider catalog for command contexts.
    pub(crate) catalog: std::sync::RwLock<Option<dal_provider::Catalog>>,
    /// Non-plugin extension names in startup order; reloads preserve them.
    pub(crate) plugin_base: Vec<Box<str>>,
}

impl HostShared {
    /// Stores the latest fetched provider catalog for command contexts.
    pub(crate) fn cache_catalog(&self, catalog: dal_provider::Catalog) {
        if let Ok(mut guard) = self.catalog.write() {
            *guard = Some(catalog);
        }
    }

    /// Returns the latest fetched provider catalog, when one exists.
    pub(crate) fn cached_catalog(&self) -> Option<dal_provider::Catalog> {
        self.catalog.read().ok().and_then(|guard| guard.clone())
    }

    /// Non-plugin extension names in canonical order at startup publication.
    pub(crate) fn base_names(generation: &Generation) -> Vec<Box<str>> {
        generation
            .extensions
            .iter()
            .filter(|ext| ext.origin() != dal_core::Origin::User)
            .map(|ext| ext.name().into())
            .collect()
    }
}

/// One host-owned live session.
pub(crate) struct SessionEntry {
    /// The actor port; a second open rebinds it to the new client.
    pub(crate) handle: crate::session::SessionHandle,
    /// The shared snapshot the actor publishes.
    pub(crate) shared: Arc<crate::session::shared::Shared>,
    /// The session request broker.
    pub(crate) broker: Arc<crate::broker::Broker>,
    /// The capability-scoped services published at session start.
    pub(crate) services: Arc<dyn crate::ext::Services>,
    /// The session-owned background tasks; close drains them last.
    pub(crate) tasks: crate::session::tasks::SessionTasks,
    /// The session workspace.
    pub(crate) workspace: Workspace,
    /// The reservation that keeps this session's display name unique while live.
    pub(crate) _name_claim: Option<NameClaim>,
    /// The child depth, zero for top-level sessions.
    pub(crate) depth: u32,
    /// The parent session, when this session is a subagent.
    pub(crate) parent: Option<SessionId>,
    /// The session generation.
    pub(crate) generation: dal_core::Gen,
    /// The turn-driver task consuming the session's driver ports.
    pub(crate) driver: JoinHandle<()>,
    /// The actor task.
    pub(crate) task: JoinHandle<()>,
    /// The session cancellation token shared by backend and driver.
    pub(crate) cancel: tokio_util::sync::CancellationToken,
    /// The session backend; close mints the session-end script host against it.
    pub(crate) backend: Arc<Backend>,
    /// The session tool overlay; close clears it.
    pub(crate) overlay: Arc<crate::ext::overlay::Overlay>,
    /// The turn-bypass cell shared with the actor and driver.
    pub(crate) control: Arc<std::sync::Mutex<crate::session::control::ControlCell>>,
    /// The member report has been taken: a set flag ends the member for
    /// mailbox addressing even though its session stays listable.
    pub(crate) reported: std::sync::atomic::AtomicBool,
    /// Whether the one grace prompt has already been consumed.
    pub(crate) prompted: std::sync::atomic::AtomicBool,
}

/// The architecture product passed to [`Host::start`].
///
/// Binary identity is supplied separately by the product factory.
pub struct Product {
    /// Product name (`"dal"` or `"dalgona"`).
    pub name: &'static str,
    /// Owned data root for stores and indexes.
    pub data_root: PathBuf,
    /// Embedded product-default TOML layered after dal code defaults.
    pub defaults: &'static str,
    /// Constructed extensions in fixed order.
    pub extensions: Vec<crate::ext::Extension>,
    /// Embedded plugin source trees.
    pub bundled: Vec<dal_core::PluginSource>,
}
