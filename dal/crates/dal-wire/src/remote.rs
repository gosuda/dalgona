//! Remote host and agent adapters over the version-1 protocol.
//!
//! `RemoteHost` speaks the same public operations as the in-process host
//! through `initialize` (`clientInfo.name = "dal-remotehost"`, the crate
//! version, and the six capabilities) over a local socket or WebSocket.
//! Connection loss cancels no server work: the client reconnects with
//! exponential delays (100 ms doubling to 30 s, resetting on success),
//! resubscribes from the last delivered `(gen, seq)`, and repaints from
//! `session/view` on `resync`. Unsupported host operations return
//! [`WireError::Unsupported`](crate::error::WireError::Unsupported) instead
//! of a silent emulation.
//!
//! One connection serves every caller without a background task: whichever
//! caller is waiting drives the shared reader, and the in-progress read or
//! reconnect is kept in shared state so a cancelled caller loses no frame.

mod agent;
mod conn;
mod decode;
mod host;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dal_core::{SessionId, SessionInfo, Update, View};

pub use agent::{RemoteAgent, RemoteSubscription};
pub use host::{RemoteHost, RemoteHostSubscription};

/// One remote endpoint.
#[derive(Clone, Debug)]
pub enum RemoteEndpoint {
    /// A local socket path.
    LocalSocket(PathBuf),
    /// A WebSocket URL.
    WebSocket(url::Url),
}

/// One delivery on a remote session subscription.
#[derive(Clone, Debug)]
pub enum RemoteDelivery {
    /// A session update; each `(gen, seq)` is delivered at most once.
    Update(Arc<Update>),
    /// The server reported a gap; the view is the repainted head and the
    /// subscription continues after its `(gen, seq)`.
    Resync(Box<View>),
}

/// One delivery on a remote host subscription.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum RemoteHostUpdate {
    /// A session was opened or its visible metadata changed.
    SessionChanged(Box<SessionInfo>),
    /// A session was closed.
    SessionRemoved(SessionId),
    /// A child session started.
    ChildStarted {
        /// The child session.
        session: SessionId,
        /// Its parent session.
        parent: SessionId,
    },
    /// A child session ended.
    ChildEnded {
        /// The child session.
        session: SessionId,
        /// Its parent session.
        parent: SessionId,
    },
    /// A provider login finished.
    LoginFinished {
        /// The provider id.
        provider: String,
        /// True when the credential is ready.
        ready: bool,
        /// The failure detail, when one was reported.
        detail: Option<String>,
    },
    /// The connection was re-established; updates during the gap were not
    /// observed, so session lists should be refreshed.
    Reconnected,
}

/// One `models/list` display row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteModel {
    /// The wire model id.
    pub id: String,
    /// The provider label.
    pub provider: String,
    /// The display name.
    pub name: String,
    /// The context window, when known.
    pub context_window: Option<u32>,
}

/// One `auth/status` provider row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteAuthRow {
    /// The provider id.
    pub provider: String,
    /// `ready`, `not_configured`, or `expired`.
    pub state: String,
    /// The stored credential kind (`api_key` or `oauth`) when `auth.json`
    /// holds one, and absent when the provider is ready from the environment
    /// or not configured.
    pub detail: Option<String>,
}

/// One `auth/login` method.
pub enum RemoteLoginMethod {
    /// Stores the given API key.
    ApiKey(String),
    /// Runs the browser OAuth flow.
    Browser,
    /// Runs the device-code OAuth flow.
    Device,
}

impl std::fmt::Debug for RemoteLoginMethod {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ApiKey(_) => "ApiKey(..)",
            Self::Browser => "Browser",
            Self::Device => "Device",
        })
    }
}

/// One `auth/login` outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteLogin {
    /// The credential is stored and ready.
    Ready,
    /// The flow waits on the user; completion arrives as
    /// [`RemoteHostUpdate::LoginFinished`].
    Pending {
        /// The URL to open.
        url: String,
        /// The device code to enter, for device flows.
        user_code: Option<String>,
    },
}

/// First reconnect delay.
const BACKOFF_BASE: Duration = Duration::from_millis(100);
/// Largest reconnect delay.
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Returns the reconnect delay before retry `attempt` (0-based): 100 ms
/// doubling up to 30 s.
pub(crate) fn backoff(attempt: u32) -> Duration {
    BACKOFF_BASE
        .checked_mul(1_u32.checked_shl(attempt).unwrap_or(u32::MAX))
        .map_or(BACKOFF_MAX, |delay| delay.min(BACKOFF_MAX))
}
