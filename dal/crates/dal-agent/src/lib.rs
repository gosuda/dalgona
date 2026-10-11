//! Session ownership, durable event processing, and host operations for dal.
//!
//! A single actor owns each live session. It applies the core fold, persists
//! state-changing records before publishing updates, and provides one
//! operation contract to local and remote clients.

mod admission;
mod agent;
mod broker;
pub mod error;
pub mod ext;
mod host;
pub(crate) mod jobs;
#[cfg(any(test, feature = "test-support"))]
pub mod login_fake;
pub(crate) mod proc;
mod scheme;
pub(crate) mod session;

pub use agent::{Agent, AnswerScope, Delivery, Subscription};
pub use broker::Broker;
pub use error::{AgentError, DenyReason, HostError, ServiceError, ToolError, ValidationError};
pub use ext::grants::{GrantKey, GrantStore, GrantStoreError, PersistentGrant};
pub use host::{
    DocEntry, Env, Host, HostSubscription, HostUpdate, Product, SessionRef, ShutdownReport,
};
pub use proc::sandbox::sandbox_notice;
pub use proc::{
    FULL_OUTPUT_PREFIX, OUTPUT_FILE_CAP_BYTES, PREVIEW_BYTES, PROGRESS_LINES, PROGRESS_PERIOD,
    Proc, ProcResult, ProcStatus, SpawnOpts, StopReason, TRUNCATION_MARKER,
};
pub use session::backend::canonicalize_existing_prefix;

/// The sign-in vocabulary of [`Host::login`], [`Host::logout`], and
/// [`Host::stored_credentials`], so front ends need no provider crate.
pub mod login {
    pub use crate::host::LoginId;
    pub use crate::host::LoginOutcome;
    pub use dal_provider::{
        CredentialKind, LoginIo, LoginProgress, Method, PASTE_HINT, PROGRESS_CAPACITY,
        SecretString, StoredCredential, find as find_provider, login_providers,
    };
    /// The token that cancels a [`LoginIo`].
    pub use tokio_util::sync::CancellationToken;

    /// Loopback endpoints for tests that run every flow against a local
    /// server through [`crate::Host::set_login_endpoints`].
    #[cfg(any(test, feature = "test-support"))]
    pub use dal_provider::LoginEndpoints;
}

/// Windows gate probe: number of live `Proc` objects. A stable nonzero
/// count after shutdown means the leak lives in retained process state.
#[cfg(windows)]
pub(crate) static LIVE_PROCS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Returns the number of live `Proc` objects (gate diagnostics only).
#[cfg(windows)]
#[doc(hidden)]
pub fn live_procs() -> usize {
    LIVE_PROCS.load(std::sync::atomic::Ordering::Relaxed)
}
