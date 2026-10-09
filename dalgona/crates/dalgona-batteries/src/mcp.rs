// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! MCP client battery: transports, per-session server lifecycle, mapped tools.

use std::{path::PathBuf, sync::Arc, time::Duration};

pub(crate) mod client;
pub(crate) mod http;
pub(crate) mod stdio;
pub(crate) mod tools;

#[cfg(test)]
mod tests;

/// Maximum time for the modern-protocol discovery probe.
pub const DISCOVER_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum time to spawn or connect to a declared server.
pub const START_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum time to list all tools from a server.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(15);
/// Initial deadline for one MCP call.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Maximum total time for one MCP call, including progress extensions.
pub const CALL_MAX: Duration = Duration::from_secs(600);
/// Maximum time for one OAuth step-up authorization.
pub const STEPUP_TIMEOUT: Duration = Duration::from_secs(300);
/// Grace period before killing a stopped stdio server process tree.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
/// Number of server restarts allowed per session and isolation key.
pub const RESTART_BUDGET: u32 = 1;
/// Number of protocol retries allowed after the first request.
pub const MRTR_MAX: u32 = 4;
/// Maximum number of OAuth scope step-ups per call.
pub const STEPUP_MAX: u32 = 2;
/// Maximum number of tools/list pages accepted from one server.
pub const LIST_PAGE_MAX: usize = 50;
/// Maximum amount of child stderr retained for crash diagnostics.
pub const STDERR_RING: usize = 65_536;
/// Default time-to-live for a tools/list response.
pub const TOOL_CACHE_DEFAULT: Duration = Duration::from_secs(60);
/// Maximum time-to-live accepted for a tools/list response.
pub const TOOL_CACHE_CAP: Duration = Duration::from_secs(3600);
/// Maximum number of bytes returned as MCP result text.
pub const RESULT_TEXT_CAP: usize = 524_288;
/// Marker appended when an MCP result exceeds [`RESULT_TEXT_CAP`].
pub const RESULT_TRUNCATED_MARKER: &str = "<mcp result truncated at 524288 bytes>";

/// Client identity and token location supplied by the Dalgona product builder.
#[derive(Clone, Debug)]
pub struct McpConfig {
    /// `<data root>/mcp/tokens.json`, written atomically with mode 0600.
    pub tokens_path: PathBuf,
    /// Dalgona workspace package version sent in MCP client metadata.
    pub client_version: String,
}

/// An MCP battery failure rendered byte-for-byte at the public boundary.
#[non_exhaustive]
#[derive(Debug, Clone, thiserror::Error)]
pub enum McpError {
    /// The user declined the full MCP server-set grant.
    #[error("mcp service declined by user for {plugin}")]
    Declined { plugin: String },
    /// The caller requested an undeclared server.
    #[error("mcp server {key} is not granted")]
    NotGranted { key: String },
    /// A declared server could not be started.
    #[error("mcp server {key} failed to start: {cause}")]
    Start { key: String, cause: String },
    /// A stdio server exited while a request was in flight.
    #[error("mcp server {key} exited during the call with status {code}")]
    Exited { key: String, code: i32 },
    /// A stdout protocol line was malformed and the server was treated as crashed.
    #[error("mcp server {key} wrote an invalid protocol line; treated as a crash")]
    InvalidLine { key: String },
    /// The server exhausted its one-restart budget and is latched off for this session.
    #[error("mcp server {key} crashed twice in this session; it stays off until the session ends")]
    Latched { key: String },
    /// An MCP call exceeded its effective deadline.
    #[error("mcp call timed out after {n} s")]
    Timeout { n: u64 },
    /// The requested remote tool is not in the server's current list.
    #[error("mcp tool {tool} not found on server {key}")]
    NotFound { tool: String, key: String },
    /// The server returned a JSON-RPC error response.
    #[error("mcp protocol error {code}: {message}")]
    Protocol { code: i64, message: String },
    /// The server returned an unsupported MCP result type.
    #[error("unsupported mcp resultType {value}")]
    ResultType { value: String },
    /// The server exceeded the tools/list page limit.
    #[error("mcp list exceeded 50 pages")]
    ListPages,
    /// Elicitation needs a front end that can answer questions.
    #[error("mcp elicitation needs an ask front end")]
    NoAskFrontEnd,
    /// OAuth issuer metadata did not match the selected authorization server.
    #[error("mcp authorization failed: issuer mismatch")]
    IssuerMismatch,
    /// OAuth authorization reached the configured scope step-up limit.
    #[error("mcp authorization failed: step-up limit reached")]
    StepUpLimit,
    /// OAuth authorization failed for another reason.
    #[error("mcp authorization failed: {cause}")]
    Auth { cause: String },
    /// The HTTP endpoint continued rejecting requests after authorization.
    #[error("mcp http status {code} after {n} authorization attempts")]
    HttpAuth { code: u16, n: u32 },
    /// The server repeatedly requested interactive input.
    #[error("mcp input-required limit reached")]
    InputRequiredLimit,
}

/// Transport-internal completion that never becomes a public error string.
#[derive(Debug, Clone)]
pub(crate) enum TransportError {
    /// A public protocol failure.
    Mcp(McpError),
    /// The owning session cancelled the operation.
    Cancelled,
}

impl From<McpError> for TransportError {
    fn from(error: McpError) -> Self {
        Self::Mcp(error)
    }
}

/// A `[plugin.mcp]` section value failed strict validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpConfigError {
    /// The section is not a TOML table.
    #[error("plugin.mcp must be a table")]
    InvalidSection,
    /// The section names a key MCP does not read.
    #[error("unknown key \"plugin.mcp.{key}\"; MCP has no user settings")]
    UnknownKey { key: Box<str> },
    /// The shared `enabled` switch is not a boolean.
    #[error("plugin.mcp.enabled must be a boolean")]
    InvalidEnabled,
}

/// The validated `[plugin.mcp]` section. MCP has no user settings; the
/// section exists only so a shared `enabled` switch and strict unknown-key
/// rejection have a typed home. Server declarations come from skill
/// frontmatter, and runtime paths come from [`McpConfig`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpSettings {
    /// Whether the battery is enabled. The entry builder also honors
    /// `disabled_batteries`; this field is the section-level switch.
    pub enabled: bool,
}

impl McpSettings {
    /// Validates the raw `[plugin.mcp]` section with unknown-key rejection.
    ///
    /// # Errors
    /// Returns [`McpConfigError`] when the section is not a table, names an
    /// unknown key, or carries a non-boolean `enabled`.
    pub fn parse(section: Option<&toml::Value>) -> Result<Self, McpConfigError> {
        let Some(section) = section else {
            return Ok(Self { enabled: true });
        };
        let toml::Value::Table(table) = section else {
            return Err(McpConfigError::InvalidSection);
        };
        let mut enabled = true;
        for (key, value) in table {
            if key == "enabled" {
                let toml::Value::Boolean(value) = value else {
                    return Err(McpConfigError::InvalidEnabled);
                };
                enabled = *value;
                continue;
            }
            return Err(McpConfigError::UnknownKey {
                key: key.as_str().into(),
            });
        }
        Ok(Self { enabled })
    }
}

/// Runtime budgets. Tests can only replace these with smaller durations.
#[derive(Clone, Debug)]
pub(crate) struct Budgets {
    pub(crate) discover: Duration,
    pub(crate) start: Duration,
    pub(crate) list: Duration,
    pub(crate) call: Duration,
    pub(crate) call_max: Duration,
    pub(crate) stepup: Duration,
    pub(crate) shutdown_grace: Duration,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            discover: DISCOVER_TIMEOUT,
            start: START_TIMEOUT,
            list: LIST_TIMEOUT,
            call: CALL_TIMEOUT,
            call_max: CALL_MAX,
            stepup: STEPUP_TIMEOUT,
            shutdown_grace: SHUTDOWN_GRACE,
        }
    }
}

/// Builds the MCP battery extension. Registers the `mcp_client` record and
/// the session lifecycle hooks. Registration never spawns a child, opens a
/// connection, or asks a question.
///
/// # Errors
/// Returns [`dal_core::RegistrationError`] when the battery name, version,
/// inject set, command name, or client record is invalid.
pub fn mcp(cfg: &McpConfig) -> Result<dal_agent::ext::Extension, dal_core::RegistrationError> {
    use dal_agent::ext::{ExtensionBuilder, McpClient};
    use dal_core::{Origin, ServiceSet};

    let inject = ServiceSet::from_names(["mcp", "env", "ask"])?;
    let client = client::Client::new(cfg.clone(), Budgets::default());
    let mcp_client: Arc<dyn McpClient> = client.clone();
    ExtensionBuilder::new("mcp", env!("CARGO_PKG_VERSION"), inject)?
        .with_origin(Origin::Bundled, None)
        .mcp_client(mcp_client)
        .on_session_start_lossless(client::SessionStartHook(Arc::clone(&client)))
        .on_session_end_lossless(client::SessionEndHook(Arc::clone(&client)))
        .command(
            dal_core::CommandSpec {
                name: dal_core::CommandName::parse("mcp")?,
                summary: "Show MCP servers in this session.".into(),
                args_hint: None,
            },
            Arc::new(client::McpCommand(client)),
        )
        .build()
}

/// The text of the `dalgona://mcp` manual page.
pub const MCP_DOC: &str = concat!(
    "# mcp\n\n",
    "The MCP client battery maps declared server tools into the session.\n",
    "Declarations come from skill frontmatter; nothing is reachable until the\n",
    "session grants the full declared server set and the call passes the normal\n",
    "approval ladder.\n\n",
    "stdio and streamable-HTTP transports are supported, on protocol revisions\n",
    "2026-07-28 and 2025-11-25. Mapped tools stay deferred to the model and are\n",
    "promoted at a turn boundary under `<skill>.<server>.<tool>` names. Calls\n",
    "run on the exec-class approval ladder; HTTP servers authenticate through\n",
    "OAuth with issuer and resource binding. The client speaks the tools-only\n",
    "protocol subset; there is no HTTP+SSE-only transport.\n\n",
    "Limits: discovery 5 s, start 10 s, list 15 s, call 60 s (600 s with\n",
    "progress), one restart per session and isolation key, 50 list pages,\n",
    "result text capped at 524288 bytes. Token files are stored at mode 0600.\n",
);
