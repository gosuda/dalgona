//! Plain values shared by extension registration, hooks, and runtime operations.

use std::{fmt, str::FromStr, sync::Arc, time::Duration};

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::content::Part;
use crate::id::{CallId, EntryId, JobId, SessionId, TurnId};
use crate::journal::JobOutcome;
use crate::model::{Caps, ModelInfo, RequestParams, Stop};
use crate::raw::{RawJson, Tagged};
use crate::workspace::Workspace;

/// A checked extension identifier with the grammar `[a-z][a-z0-9_-]{0,63}`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Name(Box<str>);

impl Name {
    /// Parses one extension identifier.
    ///
    /// # Errors
    /// Returns [`RegistrationError::InvalidName`] if the value is empty, too
    /// long, non-ASCII, or outside the identifier grammar.
    pub fn parse(value: &str) -> Result<Self, RegistrationError> {
        let valid = value.len() <= 64 && {
            let mut bytes = value.bytes();
            matches!(bytes.next(), Some(b'a'..=b'z'))
                && bytes.all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'_'
                        || byte == b'-'
                })
        };
        if valid {
            Ok(Self(value.into()))
        } else {
            Err(RegistrationError::InvalidName { name: value.into() })
        }
    }

    /// Returns the validated identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Name {
    type Err = RegistrationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for Name {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Box::<str>::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

/// The origin class of an extension declaration.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Supplied by the application itself.
    Builtin,
    /// Shipped as part of a bundled distribution.
    Bundled,
    /// Supplied by the user.
    User,
}

/// Controls where an extension-provided item is visible.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Visible to the model in its ordinary tool surface.
    Model,
    /// Available only when explicitly requested.
    Deferred,
    /// Available only to evaluation flows.
    EvalOnly,
}

impl Visibility {
    /// Parses a visibility literal.
    ///
    /// # Errors
    /// Returns [`RegistrationError::InvalidVisibility`] for any other value.
    pub fn parse(value: &str) -> Result<Self, RegistrationError> {
        match value {
            "model" => Ok(Self::Model),
            "deferred" => Ok(Self::Deferred),
            "eval_only" => Ok(Self::EvalOnly),
            _ => Err(RegistrationError::InvalidVisibility {
                value: value.into(),
            }),
        }
    }
}

/// A service named in an extension's requested injection set.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Service {
    /// Read files through the host file service.
    #[serde(rename = "fs.read")]
    FsRead,
    /// Write files through the host file service.
    #[serde(rename = "fs.write")]
    FsWrite,
    /// Make network requests through the host service.
    Net,
    /// Run a process through the host service.
    Run,
    /// Read environment values through the host service.
    Env,
    /// Ask the user through the host service.
    Ask,
    /// Access registered MCP services.
    Mcp,
    /// Start and communicate with child agents.
    Agents,
    /// Start and inspect background jobs.
    Jobs,
    /// Steer or cancel the current turn.
    Turn,
    /// Read or write extension sidecar data.
    Sidecar,
    /// Invoke model inference from an extension.
    Infer,
}

impl Service {
    /// Every service in the canonical vocabulary order.
    pub const ALL: [Self; 12] = [
        Self::FsRead,
        Self::FsWrite,
        Self::Net,
        Self::Run,
        Self::Env,
        Self::Ask,
        Self::Mcp,
        Self::Agents,
        Self::Jobs,
        Self::Turn,
        Self::Sidecar,
        Self::Infer,
    ];

    /// Returns the service's literal injection name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FsRead => "fs.read",
            Self::FsWrite => "fs.write",
            Self::Net => "net",
            Self::Run => "run",
            Self::Env => "env",
            Self::Ask => "ask",
            Self::Mcp => "mcp",
            Self::Agents => "agents",
            Self::Jobs => "jobs",
            Self::Turn => "turn",
            Self::Sidecar => "sidecar",
            Self::Infer => "infer",
        }
    }

    /// Parses a service's literal injection name.
    ///
    /// # Errors
    /// Returns [`RegistrationError::UnknownService`] for an unknown literal.
    pub fn parse(value: &str) -> Result<Self, RegistrationError> {
        Self::ALL
            .into_iter()
            .find(|service| service.as_str() == value)
            .ok_or_else(|| RegistrationError::UnknownService {
                service: value.into(),
            })
    }

    /// Returns the grant capability requested by this service, if it has one.
    #[must_use]
    pub const fn capability(self) -> Option<Capability> {
        match self {
            Self::Ask => None,
            Self::FsRead => Some(Capability::FsRead),
            Self::FsWrite => Some(Capability::FsWrite),
            Self::Net => Some(Capability::Net),
            Self::Run => Some(Capability::Run),
            Self::Env => Some(Capability::Env),
            Self::Mcp => Some(Capability::Mcp),
            Self::Agents => Some(Capability::Agents),
            Self::Jobs => Some(Capability::Jobs),
            Self::Turn => Some(Capability::Turn),
            Self::Sidecar => Some(Capability::Sidecar),
            Self::Infer => Some(Capability::Infer),
        }
    }

    const fn mask(self) -> u16 {
        1 << self as u16
    }
}

impl fmt::Display for Service {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A capability that may be granted for a requested service.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Read files through the host file service.
    #[serde(rename = "fs.read")]
    FsRead,
    /// Write files through the host file service.
    #[serde(rename = "fs.write")]
    FsWrite,
    /// Make network requests through the host service.
    Net,
    /// Run a process through the host service.
    Run,
    /// Read environment values through the host service.
    Env,
    /// Access registered MCP services.
    Mcp,
    /// Start and communicate with child agents.
    Agents,
    /// Start and inspect background jobs.
    Jobs,
    /// Steer or cancel the current turn.
    Turn,
    /// Read or write extension sidecar data.
    Sidecar,
    /// Invoke model inference from an extension.
    Infer,
}

/// A requested set of services, distinct from an authorization grant.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ServiceSet {
    bits: u16,
}

impl ServiceSet {
    /// The empty request set.
    pub const EMPTY: Self = Self { bits: 0 };

    /// Parses service names without silently accepting duplicates.
    ///
    /// # Errors
    /// Returns an unknown-service or duplicate-service registration error.
    pub fn from_names<'a>(
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, RegistrationError> {
        let mut result = Self::EMPTY;
        for name in names {
            let service = Service::parse(name)?;
            let bit = service.mask();
            if result.bits & bit != 0 {
                return Err(RegistrationError::DuplicateService { service });
            }
            result.bits |= bit;
        }
        Ok(result)
    }

    /// Reports whether this request contains a service.
    #[must_use]
    pub const fn contains(self, service: Service) -> bool {
        self.bits & service.mask() != 0
    }

    /// Removes the user-prompt service while retaining all grantable services.
    #[must_use]
    pub const fn capabilities(self) -> Self {
        Self {
            bits: self.bits & !Service::Ask.mask(),
        }
    }

    /// Iterates over requested services in [`Service::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = Service> {
        Service::ALL
            .into_iter()
            .filter(move |service| self.contains(*service))
    }

    /// Reports whether no service was requested.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }
}

/// A source location reported by an extension loader.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Site {
    /// The path containing the declaration.
    pub path: std::path::PathBuf,
    /// The one-based source line.
    pub line: u32,
    /// The one-based source column.
    pub col: u32,
}

/// A shared tool declaration with its schema retained as raw JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    /// The tool's registered name.
    pub name: Name,
    /// A human-readable description of the tool.
    pub description: Box<str>,
    /// The input schema, preserved as raw JSON.
    pub parameters: RawJson,
}

/// Metadata for a host command exposed by an extension.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
    /// The command's registered name.
    pub name: Name,
    /// A short command summary.
    pub summary: Box<str>,
    /// Optional argument guidance for command consumers.
    pub args_hint: Option<Box<str>>,
}

/// A named skill's immutable descriptive content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SkillRecord {
    /// The skill's registered name.
    pub name: Name,
    /// A short skill description.
    pub description: Box<str>,
    /// The complete skill body.
    pub body: Arc<str>,
    /// Whether the skill opts into letter-to-image handling.
    pub letter2image: bool,
}

/// The source that already owns a conflicting extension name.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Claimant {
    /// A previously registered plugin.
    Plugin(Name),
    /// A previously registered battery.
    Battery(Name),
    /// A built-in extension.
    Builtin(Name),
    /// A built-in command.
    BuiltinCommand(Name),
}

impl fmt::Display for Claimant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plugin(name) => write!(formatter, "plugin \"{name}\""),
            Self::Battery(name) => write!(formatter, "battery \"{name}\""),
            Self::Builtin(name) => write!(formatter, "the built-in extension {name}"),
            Self::BuiltinCommand(name) => write!(formatter, "the built-in command /{name}"),
        }
    }
}

/// A registration declaration or identity is invalid or already claimed.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RegistrationError {
    /// An extension identifier does not match its grammar.
    #[error("invalid name \"{name}\"; names must match [a-z][a-z0-9_-]{{0,63}}")]
    InvalidName {
        /// The rejected identifier.
        name: Box<str>,
    },
    /// A version string is not a `SemVer` version.
    #[error("version \"{version}\" is not a valid SemVer version")]
    InvalidVersion {
        /// The rejected version.
        version: Box<str>,
    },
    /// A requested service is not in the service vocabulary.
    #[error(
        "unknown service \"{service}\"; the service vocabulary is fs.read, fs.write, net, run, env, ask, mcp, agents, jobs, turn, sidecar, infer"
    )]
    UnknownService {
        /// The rejected service name.
        service: Box<str>,
    },
    /// A service occurs more than once in the requested set.
    #[error("service \"{service}\" appears twice in inject")]
    DuplicateService {
        /// The repeated service.
        service: Service,
    },
    /// A visibility literal is not supported.
    #[error("visibility must be one of \"model\", \"deferred\", \"eval_only\"; got \"{value}\"")]
    InvalidVisibility {
        /// The rejected visibility literal.
        value: Box<str>,
    },
    /// A tool's input parameters do not describe an object schema.
    #[error("parameters must be a JSON object schema ({{\"type\": \"object\", ...}})")]
    InvalidParameters,
    /// A declaration name is already owned by another extension.
    #[error("{kind} \"{name}\" is already registered by {claimant}")]
    Conflict {
        /// The kind of declaration that conflicts.
        kind: &'static str,
        /// The conflicting declaration name.
        name: Name,
        /// The record that already owns the name.
        claimant: Claimant,
    },
    /// A plugin attempted to declare a prompt section more than once.
    #[error("dal.prompt_section may be called once per plugin")]
    DuplicatePromptSection,
    /// An extension attempted to register a status kind more than once.
    #[error("status update kind may be registered once per extension")]
    DuplicateStatusKind,
    /// A rule declaration failed one of its local value checks.
    #[error("rule \"{name}\" {reason}")]
    InvalidRule {
        /// The rule name.
        name: Name,
        /// The validation failure in concise user-facing form.
        reason: Box<str>,
    },
}

/// Checks a semantic version without allocating or accepting non-SemVer text.
#[must_use]
pub fn valid_version(version: &str) -> bool {
    let (version, build) = match version.split_once('+') {
        Some((version, build)) if !build.contains('+') => (version, Some(build)),
        Some(_) => return false,
        None => (version, None),
    };
    if let Some(build) = build
        && !valid_identifiers(build, false)
    {
        return false;
    }

    let (core, prerelease) = match version.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (version, None),
    };
    if let Some(prerelease) = prerelease
        && !valid_identifiers(prerelease, true)
    {
        return false;
    }

    let mut components = core.split('.');
    for _ in 0..3 {
        let Some(component) = components.next() else {
            return false;
        };
        if component.is_empty()
            || (component.len() > 1 && component.starts_with('0'))
            || !component.bytes().all(|byte| byte.is_ascii_digit())
            || component.parse::<u64>().is_err()
        {
            return false;
        }
    }
    components.next().is_none()
}

fn valid_identifiers(value: &str, reject_numeric_leading_zero: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && (!reject_numeric_leading_zero
                    || !identifier.bytes().all(|byte| byte.is_ascii_digit())
                    || identifier.len() == 1
                    || !identifier.starts_with('0'))
        })
}

#[derive(Deserialize)]
struct ToolSchemaType {
    #[serde(default, rename = "type", deserialize_with = "deserialize_schema_type")]
    kind: Option<Box<str>>,
}

fn deserialize_schema_type<'de, D>(deserializer: D) -> Result<Option<Box<str>>, D::Error>
where
    D: Deserializer<'de>,
{
    Box::<str>::deserialize(deserializer).map(Some)
}

/// Checks whether raw JSON is an object schema with an optional object type.
#[must_use]
pub fn valid_tool_parameters(value: &RawJson) -> bool {
    let text = value.as_str();
    if !text.starts_with('{') {
        return false;
    }
    let Ok(schema) = sonic_rs::from_str::<ToolSchemaType>(text) else {
        return false;
    };
    schema.kind.as_deref().is_none_or(|kind| kind == "object")
}

/// A rule's interrupt behavior when a matching condition is found.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum InterruptMode {
    /// Interrupt for any matching content.
    #[serde(rename = "always")]
    Always,
    /// Interrupt only for prose content.
    #[serde(rename = "prose-only")]
    ProseOnly,
    /// Interrupt only for tool arguments.
    #[serde(rename = "tool-only")]
    ToolOnly,
    /// Never interrupt for this rule.
    #[serde(rename = "never")]
    Never,
}

/// A rule's repeated-match policy.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    /// Apply only once.
    Once,
    /// Apply again after its configured gap.
    AfterGap,
}

/// Selects stream content classes and optional tool names for a rule.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    /// Whether assistant text is included.
    pub text: bool,
    /// Whether reasoning text is included.
    pub thinking: bool,
    /// Whether tool arguments are included.
    pub tool: bool,
    /// Optional restriction to named tools.
    pub named_tools: Vec<Name>,
}

/// A rule declaration whose patterns are checked when a rules set is built.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RuleRecord {
    /// The rule's checked identifier.
    pub name: Name,
    /// One to sixteen raw regular-expression sources.
    pub patterns: Vec<Box<str>>,
    /// The reminder or instruction text to inject.
    pub text: Box<str>,
    /// An optional judge instruction.
    pub judge: Option<Box<str>>,
    /// Optional content selection; absence preserves the rules default.
    pub scope: Option<Scope>,
    /// Optional path globs that select matching files.
    pub globs: Option<Vec<Box<str>>>,
    /// Optional agent names that select this rule.
    pub agents: Option<Vec<Box<str>>>,
    /// Optional interruption behavior.
    pub mode: Option<InterruptMode>,
    /// Optional repeated-match behavior.
    pub repeat_mode: Option<RepeatMode>,
    /// Optional number of deltas between repeated applications.
    pub repeat_gap: Option<u16>,
    /// Whether the rule applies without a match report.
    pub always_apply: bool,
    /// Whether matches are reported.
    pub report: bool,
    /// Whether the rule is active.
    pub enabled: bool,
}

impl RuleRecord {
    /// Validates the declaration's pattern count and repeat gap.
    ///
    /// Pattern text is intentionally not compiled here; rule-set construction
    /// owns matcher limits and malformed-pattern handling.
    ///
    /// # Errors
    /// Returns [`RegistrationError::InvalidRule`] for an invalid pattern count
    /// or repeat gap.
    pub fn validate(&self) -> Result<(), RegistrationError> {
        if !(1..=16).contains(&self.patterns.len()) {
            return Err(RegistrationError::InvalidRule {
                name: self.name.clone(),
                reason: "must declare one to sixteen patterns".into(),
            });
        }
        if self
            .repeat_gap
            .is_some_and(|gap| !(1..=1000).contains(&gap))
        {
            return Err(RegistrationError::InvalidRule {
                name: self.name.clone(),
                reason: "repeat_gap must be within 1..=1000".into(),
            });
        }
        Ok(())
    }
}

/// A stream channel used by Rust stream watchers.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Channel {
    /// Assistant-visible text.
    Text,
    /// Reasoning text.
    Thinking,
    /// Arguments for a named tool.
    ToolArgs {
        /// The tool whose arguments are streamed.
        tool: Name,
    },
}

/// A stream watcher's decision for the current delta.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum StreamVerdict {
    /// Keep processing without injecting a reminder.
    Continue,
    /// Interrupt with a named rule and reminder text.
    Interrupt {
        /// The rule that matched.
        rule: Box<str>,
        /// The reminder to inject.
        inject: Box<str>,
    },
}

/// Information available when a session starts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionStart {
    /// The session that started.
    pub session: SessionId,
    /// The session's workspace.
    pub workspace: Workspace,
    /// Whether this session resumed prior state.
    pub resumed: bool,
}

/// Information available when a session ends.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionEnd {
    /// The session that ended.
    pub session: SessionId,
    /// Why the session ended.
    pub reason: Box<str>,
}

/// Input text and content supplied to input hooks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct InputEvent {
    /// The input content, including any image or blob parts.
    pub content: Vec<Part>,
}

/// Context supplied to a before-turn hook.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BeforeTurn {
    /// The active turn.
    pub turn: TurnId,
    /// The prompt text for this turn.
    pub text: Box<str>,
}

/// Context supplied before a model request is made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BeforeRequest {
    /// The active turn.
    pub turn: TurnId,
    /// The request round within the turn.
    pub round: u32,
    /// The resolved model metadata.
    pub model: ModelInfo,
    /// The model's supported capabilities.
    pub caps: Caps,
    /// The current request tuning parameters.
    pub params: RequestParams,
}

/// Context supplied before a tool call is dispatched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEvent {
    /// The active turn.
    pub turn: TurnId,
    /// The provider's call identity.
    pub call: CallId,
    /// The checked tool name.
    pub tool: Name,
    /// Raw tool arguments, preserved without re-encoding.
    pub args: RawJson,
}

/// The outcome information made available after a tool call.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ToolResultEvent {
    /// The active turn.
    pub turn: TurnId,
    /// The provider's call identity.
    pub call: CallId,
    /// The checked tool name.
    pub tool: Name,
    /// Whether the tool call succeeded.
    pub ok: bool,
    /// A bounded human-readable result preview.
    pub preview: Box<str>,
}

/// The terminal result of a turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TurnEnd {
    /// The completed turn.
    pub turn: TurnId,
    /// Why the turn stopped.
    pub stop: Stop,
}

/// A notification that a turn has settled.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Settled {
    /// The settled turn.
    pub turn: TurnId,
}

/// The typed result of an input hook.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum InputVerdict {
    /// Continue dispatch without replacing input content.
    Continue,
    /// Replace input content with the supplied parts.
    Transform(Vec<Part>),
    /// Mark the input handled and stop input dispatch.
    Handled,
}

/// The typed result of a tool-call hook.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ToolCallVerdict {
    /// Allow the tool call with its original arguments.
    Allow,
    /// Block the tool call with a reason.
    Block {
        /// Why the tool call was blocked.
        reason: Box<str>,
    },
    /// Replace only the call arguments.
    Rewrite {
        /// The replacement arguments, preserved as raw JSON.
        args: RawJson,
    },
}

// Verdict and job ops decode through the raw tagged carrier: their
// `RawJson` members cannot pass through serde's internally tagged
// content buffering, which cannot hand raw bytes to a nested member.
#[derive(Deserialize)]
struct BlockVerdictFields {
    reason: Box<str>,
}

#[derive(Deserialize)]
struct RewriteVerdictFields {
    args: RawJson,
}

impl<'de> Deserialize<'de> for ToolCallVerdict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(deserializer, "type", &["allow", "block", "rewrite"])?;
        match tagged.kind() {
            "allow" => Ok(Self::Allow),
            "block" => {
                let wire: BlockVerdictFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Block {
                    reason: wire.reason,
                })
            }
            "rewrite" => {
                let wire: RewriteVerdictFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Rewrite { args: wire.args })
            }
            other => Err(de::Error::custom(format!("unknown verdict type `{other}`"))),
        }
    }
}

/// Names registered for Starlark hook dispatch.
pub const STAR_EVENTS: [&str; 9] = [
    "session_start",
    "session_end",
    "input",
    "before_turn",
    "before_request",
    "tool_call",
    "tool_result",
    "turn_end",
    "settled",
];

/// The event name reserved for Rust-only stream watchers.
pub const RUST_STREAM_EVENT: &str = "output_stream";

/// The error policy for a fan-out scope.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum OnError {
    /// Cancel remaining work when one member fails.
    #[default]
    Cancel,
    /// Let remaining work settle after a member fails.
    Settle,
}

/// Optional bounds reserved for a fan-out scope.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    /// Maximum number of inference requests.
    pub requests: Option<u64>,
    /// Maximum input tokens across requests.
    pub input_tokens: Option<u64>,
    /// Maximum output tokens across requests.
    pub output_tokens: Option<u64>,
    /// Maximum elapsed wall time.
    pub wall: Option<Duration>,
    /// Maximum known USD cost.
    pub usd: Option<f64>,
}

/// Fan-out configuration with an explicit member limit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScopeSpec {
    /// Maximum number of members admitted to this scope.
    pub limit: u16,
    /// How member failures affect the remaining work.
    pub on_error: OnError,
    /// Optional request, token, time, and cost bounds.
    pub budget: Budget,
}

impl ScopeSpec {
    /// Validates the member limit and all configured budget bounds.
    ///
    /// # Errors
    /// Returns [`ScopeSpecError::InvalidLimit`] when the limit is outside the
    /// global bound, or [`ScopeSpecError::InvalidBudget`] for a non-positive
    /// integer limit or an invalid USD limit.
    pub fn validate(&self, global_member_cap: u16) -> Result<(), ScopeSpecError> {
        let cap = global_member_cap.min(500);
        if self.limit == 0 || self.limit > cap {
            return Err(ScopeSpecError::InvalidLimit {
                limit: self.limit,
                cap,
            });
        }
        if self.budget.requests == Some(0)
            || self.budget.input_tokens == Some(0)
            || self.budget.output_tokens == Some(0)
            || self
                .budget
                .usd
                .is_some_and(|usd| !usd.is_finite() || usd <= 0.0)
        {
            return Err(ScopeSpecError::InvalidBudget);
        }
        Ok(())
    }
}

/// A configured scope limit or budget is invalid, exhausted, or cancelled.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScopeSpecError {
    /// The scope limit is outside the permitted range.
    #[error("scope limit {limit} is outside 1..={cap}")]
    InvalidLimit {
        /// The requested member limit.
        limit: u16,
        /// The maximum permitted member count.
        cap: u16,
    },
    /// A budget bound is not positive and finite.
    #[error("scope budget bounds must be positive and finite")]
    InvalidBudget,
    /// The selected model has no usable price.
    #[error("model \"{model}\" has no known price")]
    UnpricedModel {
        /// The model route or identifier without a known price.
        model: Box<str>,
    },
    /// A scope has consumed a configured budget bound.
    #[error("scope budget exhausted")]
    Exhausted,
    /// A scope was cancelled before completion.
    #[error("scope cancelled")]
    Cancelled,
}

/// Cumulative normalized usage charged to a fan-out scope.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScopeUsage {
    /// Number of model requests.
    pub requests: u64,
    /// Input tokens across requests.
    pub input_tokens: u64,
    /// Output tokens across requests.
    pub output_tokens: u64,
    /// Known cumulative USD cost, absent when pricing is unknown.
    pub cost_usd: Option<f64>,
}

/// The current admission and completion state of an agent handle.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HandleStatus {
    /// Waiting for capacity admission.
    Pending,
    /// Admitted and currently running.
    Running,
    /// Completed successfully.
    Done,
    /// Failed with an error.
    Failed,
    /// Cancelled before successful completion.
    Cancelled,
}

/// Configuration for starting a child agent session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentStart {
    /// The prompt for the child session.
    pub prompt: Box<str>,
    /// An optional model identifier.
    pub model: Option<Box<str>>,
    /// An optional role name.
    pub role: Option<Box<str>>,
    /// An optional explicit system prompt.
    pub system: Option<Box<str>>,
    /// Optional tool names; absence differs from an explicit empty set.
    pub tools: Option<Vec<Name>>,
    /// An optional child workspace.
    pub workspace: Option<Workspace>,
}

impl AgentStart {
    /// Checks that mutually exclusive role and system options are not combined.
    ///
    /// # Errors
    /// Returns [`AgentsOpError::RoleAndSystem`] when both options are present.
    pub fn validate(&self) -> Result<(), AgentsOpError> {
        if self.role.is_some() && self.system.is_some() {
            return Err(AgentsOpError::RoleAndSystem);
        }
        Ok(())
    }
}

/// How a mailbox message should be delivered to its recipient.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MailMode {
    /// Deliver as an aside without steering the current turn.
    Aside,
    /// Steer the currently running turn.
    Steer,
    /// Queue for the recipient's next turn.
    NextTurn,
}

/// A mailbox message exchanged between two sessions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Mail {
    /// The sending session.
    pub from: SessionId,
    /// The receiving session.
    pub to: SessionId,
    /// The requested delivery mode.
    pub mode: MailMode,
    /// The message text.
    pub text: Box<str>,
    /// The journal entry to which this message replies, if any.
    pub reply_to: Option<EntryId>,
}

/// The receipt returned after a mailbox send attempt.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Receipt {
    /// The message was delivered to the recipient.
    Delivered,
    /// Delivery woke the recipient's session.
    Woken,
    /// The message was buffered for a later turn.
    Buffered,
    /// The recipient's mailbox has no available capacity.
    Full,
    /// The recipient does not exist or has ended.
    Gone,
}

/// An agent-session operation requested by a host service.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentsOp {
    /// Starts a child agent session.
    Start(AgentStart),
    /// Waits for a child session to finish.
    Await {
        /// The child session to await.
        id: SessionId,
        /// Optional maximum wait duration.
        timeout: Option<Duration>,
    },
    /// Cancels a child session.
    Cancel {
        /// The child session to cancel.
        id: SessionId,
    },
    /// Lists child sessions.
    List,
    /// Sends a mailbox message to one session.
    Send {
        /// The recipient session.
        to: SessionId,
        /// The message text.
        text: Box<str>,
        /// The delivery mode.
        mode: MailMode,
        /// The journal entry to reply to, if any.
        reply_to: Option<EntryId>,
    },
    /// Reads mailbox messages after a journal cursor.
    Recv {
        /// The last entry already read, if any.
        after: Option<EntryId>,
        /// Optional maximum wait duration for new mail.
        timeout: Option<Duration>,
    },
    /// Sends one message to each child session.
    Broadcast {
        /// The message text.
        text: Box<str>,
        /// The delivery mode.
        mode: MailMode,
    },
}

/// The result or failure of an agent-session operation.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AgentsOpError {
    /// A start request supplied both a role and a system prompt.
    #[error("role and system cannot both be set")]
    RoleAndSystem,
    /// Agent capacity is currently full.
    #[error("agent capacity is full")]
    Full,
    /// The requested child session is unavailable.
    #[error("agent session is gone")]
    Gone,
    /// The operation is unavailable in the current context.
    #[error("agents operation is unavailable: {what}")]
    Unavailable {
        /// The operation or context that is unavailable.
        what: &'static str,
    },
    /// An operation failed with an owned message.
    #[error("agents operation failed: {message}")]
    Failed {
        /// The failure description.
        message: Box<str>,
    },
}

/// A background-job operation requested by a host service.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum JobsOp {
    /// Starts a named background job with a raw JSON payload.
    Spawn {
        /// The registered job name.
        name: Name,
        /// The job's input payload, preserved as raw JSON.
        payload: RawJson,
    },
    /// Reads the state of one background job.
    Status {
        /// The job to inspect.
        id: JobId,
    },
    /// Cancels one background job.
    Cancel {
        /// The job to cancel.
        id: JobId,
    },
}

#[derive(Deserialize)]
struct SpawnJobFields {
    name: Name,
    payload: RawJson,
}

#[derive(Deserialize)]
struct IdJobFields {
    id: JobId,
}

impl<'de> Deserialize<'de> for JobsOp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(deserializer, "type", &["spawn", "status", "cancel"])?;
        match tagged.kind() {
            "spawn" => {
                let wire: SpawnJobFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Spawn {
                    name: wire.name,
                    payload: wire.payload,
                })
            }
            "status" => {
                let wire: IdJobFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Status { id: wire.id })
            }
            "cancel" => {
                let wire: IdJobFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Cancel { id: wire.id })
            }
            other => Err(de::Error::custom(format!("unknown jobs op `{other}`"))),
        }
    }
}

/// A turn operation requested by a host service.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TurnOp {
    /// Cancels the active turn.
    Cancel,
    /// Adds steering text to the active turn.
    Steer {
        /// The text to add.
        text: Box<str>,
    },
    /// Wakes a turn with collected text and source metadata.
    Wake {
        /// The text to add to the turn.
        text: Box<str>,
        /// Descriptions of the wake sources.
        sources: Vec<Box<str>>,
        /// Background jobs associated with the wake.
        job_ids: Vec<JobId>,
    },
    /// Checks whether the active turn is idle.
    IsIdle,
}

/// A sidecar read or write operation.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SidecarOp {
    /// Reads a named sidecar value.
    Read {
        /// The sidecar name.
        name: Name,
    },
    /// Writes bytes to a named sidecar value.
    Write {
        /// The sidecar name.
        name: Name,
        /// The bytes to store.
        bytes: Vec<u8>,
    },
}

/// A process execution request passed to a host service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    /// The executable and its argument vector.
    pub argv: Vec<std::ffi::OsString>,
    /// An optional working directory.
    pub cwd: Option<std::path::PathBuf>,
    /// Optional standard-input bytes.
    pub stdin: Option<Vec<u8>>,
    /// Optional process deadline.
    pub timeout: Option<Duration>,
}

/// The collected output of a process execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RunOutput {
    /// The process exit status.
    pub status: ExitStatusKind,
    /// The retained tail of standard output.
    pub stdout_tail: Vec<u8>,
    /// The retained tail of standard error.
    pub stderr_tail: Vec<u8>,
    /// An optional path to the complete process log.
    pub log: Option<std::path::PathBuf>,
}

/// The portable exit state of a process.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ExitStatusKind {
    /// The process exited with a numeric status.
    Exited(i32),
    /// The process was terminated by a signal number.
    Signaled(i32),
    /// The configured process deadline elapsed.
    TimedOut,
    /// The process was aborted by its host.
    Aborted,
}

/// The result of an agent-session operation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentsReply {
    /// A child session was started.
    Started {
        /// The new child session.
        id: SessionId,
    },
    /// A child session finished with a report.
    Finished {
        /// The completed child session.
        id: SessionId,
        /// Its final report.
        report: String,
    },
    /// A child session was cancelled.
    Cancelled {
        /// The cancelled child session.
        id: SessionId,
    },
    /// The child sessions currently known to the host.
    Listed(Vec<SessionId>),
    /// The receipt for a mailbox send.
    Delivered(Receipt),
    /// Messages read from a mailbox.
    Received(Vec<Mail>),
}

/// The result of a background-job operation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum JobsReply {
    /// A background job was started.
    Spawned {
        /// The new job identity.
        id: JobId,
    },
    /// The current state of a background job.
    Status(JobStatus),
    /// A background job was cancelled.
    Cancelled {
        /// The cancelled job identity.
        id: JobId,
    },
}

/// A snapshot of one background job's public state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct JobStatus {
    /// The job identity.
    pub id: JobId,
    /// A display label for the job.
    pub label: Box<str>,
    /// The job's current state.
    pub state: JobStateView,
    /// An optional path to the job's log.
    pub log: Option<std::path::PathBuf>,
}

/// The public state of a background job.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum JobStateView {
    /// The job is running in the foreground.
    Running,
    /// The job continues independently of its foreground owner.
    Detached,
    /// The job completed with its journal outcome.
    Done(JobOutcome),
}

/// The result of a turn operation.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum TurnOpReply {
    /// Reports whether the turn was idle.
    Idle(bool),
    /// Steering text was queued.
    Steered,
    /// The turn was woken.
    Woken,
    /// The turn was cancelled.
    Cancelled,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn rule_record(patterns: Vec<Box<str>>, repeat_gap: Option<u16>) -> RuleRecord {
        RuleRecord {
            name: Name("rule".into()),
            patterns,
            text: "reminder".into(),
            judge: None,
            scope: None,
            globs: None,
            agents: None,
            mode: None,
            repeat_mode: None,
            repeat_gap,
            always_apply: false,
            report: false,
            enabled: true,
        }
    }

    #[test]
    fn name_boundary_and_ascii_only() -> TestResult {
        assert_eq!(Name::parse("a")?.as_str(), "a");
        let longest = "a".repeat(64);
        assert_eq!(Name::parse(&longest)?.as_str(), longest.as_str());

        let too_long = "a".repeat(65);
        for value in ["BadName", too_long.as_str(), "", "é"] {
            let error = Name::parse(value)
                .err()
                .ok_or("invalid name was accepted")?;
            assert!(matches!(error, RegistrationError::InvalidName { .. }));
            assert_eq!(
                error.to_string(),
                format!("invalid name \"{value}\"; names must match [a-z][a-z0-9_-]{{0,63}}")
            );
        }

        let legal = Name::parse("lower_name-2")?;
        let encoded = sonic_rs::to_string(&legal)?;
        assert_eq!(encoded, "\"lower_name-2\"");
        assert_eq!(sonic_rs::from_str::<Name>(&encoded)?, legal);
        assert!(sonic_rs::from_str::<Name>("\"BadName\"").is_err());
        Ok(())
    }

    #[test]
    fn service_set_has_stable_order_and_excludes_ask_from_grants() -> TestResult {
        let reverse = Service::ALL
            .into_iter()
            .rev()
            .map(Service::as_str)
            .collect::<Vec<_>>();
        let requested = ServiceSet::from_names(reverse)?;
        let expected = [
            "fs.read", "fs.write", "net", "run", "env", "ask", "mcp", "agents", "jobs", "turn",
            "sidecar", "infer",
        ];
        assert_eq!(
            requested.iter().map(Service::as_str).collect::<Vec<_>>(),
            expected
        );
        let capabilities = requested.capabilities();
        assert!(requested.contains(Service::Ask));
        assert!(!capabilities.contains(Service::Ask));
        assert!(capabilities.contains(Service::Infer));
        assert!(requested.contains(Service::Infer));
        assert!(
            !ServiceSet::from_names(["ask"])?
                .capabilities()
                .contains(Service::Ask)
        );
        assert!(ServiceSet::from_names(["ask"])?.capabilities().is_empty());

        let unknown = ServiceSet::from_names(["nope"])
            .err()
            .ok_or("unknown service accepted")?;
        assert_eq!(
            unknown.to_string(),
            "unknown service \"nope\"; the service vocabulary is fs.read, fs.write, net, run, env, ask, mcp, agents, jobs, turn, sidecar, infer"
        );
        let duplicate = ServiceSet::from_names(["run", "run"])
            .err()
            .ok_or("duplicate service accepted")?;
        assert_eq!(
            duplicate.to_string(),
            "service \"run\" appears twice in inject"
        );
        Ok(())
    }

    #[test]
    fn semver_and_object_schema_reject_invalid_inputs() -> TestResult {
        assert!(valid_version("1.0.0"));
        assert!(valid_version("1.2.3-alpha.1+build.2"));
        assert!(valid_version("0.0.0-0+00"));
        for version in [
            "x",
            "v1.2.3",
            "01.2.3",
            "1.2.3-",
            "1.2.3+",
            "1.2.3-01",
            "1.2.3+build+other",
            "1.2.3 ",
            "18446744073709551616.0.0",
        ] {
            assert!(
                !valid_version(version),
                "accepted invalid version {version:?}"
            );
        }

        let object_type = RawJson::parse(r#"{"type":"object","properties":{}}"#)?;
        let no_type = RawJson::parse("{}")?;
        let string_type = RawJson::parse(r#"{"type":"string"}"#)?;
        let null_type = RawJson::parse(r#"{"type":null}"#)?;
        let array = RawJson::parse("[]")?;
        assert!(valid_tool_parameters(&object_type));
        assert!(valid_tool_parameters(&no_type));
        assert!(!valid_tool_parameters(&string_type));
        assert!(!valid_tool_parameters(&null_type));
        assert!(!valid_tool_parameters(&array));
        assert!(RawJson::parse("{").is_err());
        assert_eq!(
            RegistrationError::InvalidParameters.to_string(),
            "parameters must be a JSON object schema ({\"type\": \"object\", ...})"
        );
        Ok(())
    }

    #[test]
    fn rule_declaration_keeps_bad_pattern_until_set_build() -> TestResult {
        let malformed_pattern = rule_record(vec!["(".into()], None);
        assert!(malformed_pattern.validate().is_ok());

        let empty = rule_record(Vec::new(), None);
        let count_error = empty.validate().err().ok_or("empty patterns accepted")?;
        assert_eq!(
            count_error.to_string(),
            "rule \"rule\" must declare one to sixteen patterns"
        );
        let too_many = rule_record(vec!["(".into(); 17], None);
        assert_eq!(
            too_many
                .validate()
                .err()
                .ok_or("seventeen patterns accepted")?
                .to_string(),
            "rule \"rule\" must declare one to sixteen patterns"
        );
        let invalid_gap = rule_record(vec!["(".into()], Some(0));
        assert_eq!(
            invalid_gap
                .validate()
                .err()
                .ok_or("zero repeat gap accepted")?
                .to_string(),
            "rule \"rule\" repeat_gap must be within 1..=1000"
        );
        Ok(())
    }

    #[test]
    fn scope_and_agent_start_validate_before_effects() -> TestResult {
        let mut spec = ScopeSpec {
            limit: 0,
            on_error: OnError::default(),
            budget: Budget::default(),
        };
        assert_eq!(
            spec.validate(500),
            Err(ScopeSpecError::InvalidLimit { limit: 0, cap: 500 })
        );
        spec.limit = 501;
        assert_eq!(
            spec.validate(500),
            Err(ScopeSpecError::InvalidLimit {
                limit: 501,
                cap: 500
            })
        );
        spec.limit = 64;
        assert_eq!(spec.validate(500), Ok(()));
        assert_eq!(spec.validate(501), Ok(()));
        assert!(spec.budget.usd.is_none());

        spec.budget.usd = Some(f64::NAN);
        assert_eq!(spec.validate(500), Err(ScopeSpecError::InvalidBudget));
        spec.budget.usd = Some(0.0);
        assert_eq!(spec.validate(500), Err(ScopeSpecError::InvalidBudget));
        spec.budget.usd = None;
        spec.budget.requests = Some(0);
        assert_eq!(spec.validate(500), Err(ScopeSpecError::InvalidBudget));

        let invalid_start = AgentStart {
            prompt: "child prompt".into(),
            model: None,
            role: Some("reviewer".into()),
            system: Some("custom system".into()),
            tools: None,
            workspace: None,
        };
        let error = invalid_start
            .validate()
            .err()
            .ok_or("role and system accepted")?;
        assert_eq!(error.to_string(), "role and system cannot both be set");
        let valid_start = AgentStart {
            role: None,
            ..invalid_start
        };
        assert_eq!(valid_start.validate(), Ok(()));
        Ok(())
    }

    #[test]
    fn mailbox_values_keep_mode_and_cursor() -> TestResult {
        let from = SessionId::new_v7();
        let to = SessionId::new_v7();
        let cursor = EntryId::new(NonZeroU64::MIN);
        for mode in [MailMode::Aside, MailMode::Steer, MailMode::NextTurn] {
            let mail = Mail {
                from,
                to,
                mode,
                text: "continue from this point".into(),
                reply_to: Some(cursor),
            };
            let encoded = sonic_rs::to_string(&mail)?;
            assert_eq!(sonic_rs::from_str::<Mail>(&encoded)?, mail);

            let send = AgentsOp::Send {
                to,
                text: "continue from this point".into(),
                mode,
                reply_to: Some(cursor),
            };
            let encoded = sonic_rs::to_string(&send)?;
            assert_eq!(sonic_rs::from_str::<AgentsOp>(&encoded)?, send);
        }

        let recv = AgentsOp::Recv {
            after: Some(cursor),
            timeout: None,
        };
        let encoded = sonic_rs::to_string(&recv)?;
        assert_eq!(sonic_rs::from_str::<AgentsOp>(&encoded)?, recv);
        Ok(())
    }

    #[test]
    fn hook_verdicts_cannot_represent_an_untyped_result() -> TestResult {
        assert_eq!(
            STAR_EVENTS,
            [
                "session_start",
                "session_end",
                "input",
                "before_turn",
                "before_request",
                "tool_call",
                "tool_result",
                "turn_end",
                "settled",
            ]
        );
        assert_eq!(RUST_STREAM_EVENT, "output_stream");

        let input_verdicts = [
            InputVerdict::Continue,
            InputVerdict::Transform(vec![Part::Text {
                text: "transformed".into(),
            }]),
            InputVerdict::Handled,
        ];
        for verdict in input_verdicts {
            let encoded = sonic_rs::to_string(&verdict)?;
            assert_eq!(sonic_rs::from_str::<InputVerdict>(&encoded)?, verdict);
        }

        let raw_args = r#"{"b": 2, "a":1e+02}"#;
        let rewrite = ToolCallVerdict::Rewrite {
            args: RawJson::parse(raw_args)?,
        };
        let encoded = sonic_rs::to_string(&rewrite)?;
        assert!(
            encoded.contains(raw_args),
            "raw arguments changed: {encoded}"
        );
        assert_eq!(sonic_rs::from_str::<ToolCallVerdict>(&encoded)?, rewrite);
        for verdict in [
            ToolCallVerdict::Allow,
            ToolCallVerdict::Block {
                reason: "denied".into(),
            },
            rewrite,
        ] {
            let encoded = sonic_rs::to_string(&verdict)?;
            assert_eq!(sonic_rs::from_str::<ToolCallVerdict>(&encoded)?, verdict);
        }

        let channels = [
            Channel::Text,
            Channel::Thinking,
            Channel::ToolArgs {
                tool: Name::parse("shell")?,
            },
        ];
        for channel in channels {
            let encoded = sonic_rs::to_string(&channel)?;
            assert_eq!(sonic_rs::from_str::<Channel>(&encoded)?, channel);
        }

        let stream_verdicts = [
            StreamVerdict::Continue,
            StreamVerdict::Interrupt {
                rule: "guard".into(),
                inject: "remember the boundary".into(),
            },
        ];
        for verdict in stream_verdicts {
            let encoded = sonic_rs::to_string(&verdict)?;
            assert_eq!(sonic_rs::from_str::<StreamVerdict>(&encoded)?, verdict);
        }
        Ok(())
    }

    #[test]
    fn jobs_ops_round_trip_with_raw_payloads() -> TestResult {
        // The spawn payload keeps its bytes verbatim through the tagged
        // decode; a byte-exact payload is the point of the member.
        let raw_payload = r#"{"script": "x",  "n":1e+03}"#;
        let spawned: JobsOp = sonic_rs::from_str(&format!(
            r#"{{"type":"spawn","name":"backup","payload":{raw_payload}}}"#
        ))?;
        let JobsOp::Spawn { name, payload } = spawned else {
            panic!("expected spawn");
        };
        assert_eq!(name.as_str(), "backup");
        assert_eq!(payload.as_str(), raw_payload);

        for op in [
            sonic_rs::to_string(&JobsOp::Status {
                id: JobId::new_v7(),
            })?,
            sonic_rs::to_string(&JobsOp::Cancel {
                id: JobId::new_v7(),
            })?,
        ] {
            assert!(sonic_rs::from_str::<JobsOp>(&op).is_ok());
        }
        assert!(sonic_rs::from_str::<JobsOp>(r#"{"type":"pause","id":"x"}"#).is_err());
        Ok(())
    }
}
