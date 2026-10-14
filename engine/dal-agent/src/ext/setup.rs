//! Fallible extension setup at session creation.

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

use dal_core::{
    Config, ModelRoute, Name, Notice, RuleFile, RuleRecord, SessionId, Site, Workspace,
};
use tokio_util::sync::CancellationToken;

use super::{BoxFuture, Caller, HookError, Services, WatchFactory};

/// Session inputs supplied to one extension's fallible setup.
///
/// The host constructs this value only after capability services are available
/// and before it observes or publishes the session. `rules` and `rule_files`
/// are snapshots from the validated generation; setup must not re-read them.
pub struct SessionCx {
    /// The session being opened.
    pub session: SessionId,
    /// The session workspace.
    pub workspace: Workspace,
    /// The parent session when this is a child session.
    pub parent: Option<SessionId>,
    /// The product identity.
    pub product: &'static str,
    /// The validated product configuration.
    pub config: Arc<Config>,
    /// Capability-scoped services for this setup.
    pub services: Arc<dyn Services>,
    /// Host-minted identity of the setup owner.
    pub caller: Caller,
    /// Session cancellation token.
    pub cancel: CancellationToken,
    /// Host data root.
    pub data_root: PathBuf,
    /// Every validated rule record, attributed to its owning extension.
    pub rules: Vec<SessionRule>,
    /// Model-visible tool names in generation order.
    pub tool_names: Vec<Box<str>>,
    /// Retained plugin rule files from this generation.
    pub rule_files: Vec<RuleFile>,
    /// The replayed requested/active model route, when one is available.
    pub model: Option<ModelRoute>,
    /// The model identity used by the host, or empty when no route is ready.
    pub model_id: Box<str>,
    /// Retained owner state from a matching parent setup, when available.
    pub inherited_state: Option<Arc<dyn Any + Send + Sync>>,
}

/// One rule record with its already-validated owner attribution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionRule {
    /// The extension that owns the record.
    pub ext: Name,
    /// The declaration site, when known.
    pub site: Option<Site>,
    /// The validated rule record.
    pub record: RuleRecord,
}

/// Installations returned by a successful extension setup.
#[derive(Default)]
pub struct SessionInstalls {
    /// Watch factories owned by the setup extension.
    pub watches: Vec<Arc<dyn WatchFactory>>,
    /// Notices buffered until a client is bound and the driver is ready.
    pub notices: Vec<Notice>,
    /// Opaque immutable state retained for this extension in this session.
    pub state: Option<Arc<dyn Any + Send + Sync>>,
}

/// Fallible setup run once for each extension that registers one.
pub trait SessionSetup: Send + Sync + 'static {
    /// Opens the extension for one session.
    fn open(&self, cx: SessionCx) -> BoxFuture<'static, Result<SessionInstalls, HookError>>;
}

impl<F> SessionSetup for F
where
    F: Fn(SessionCx) -> BoxFuture<'static, Result<SessionInstalls, HookError>>
        + Send
        + Sync
        + 'static,
{
    fn open(&self, cx: SessionCx) -> BoxFuture<'static, Result<SessionInstalls, HookError>> {
        self(cx)
    }
}
