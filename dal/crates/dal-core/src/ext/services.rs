use super::{
    Arc, CommandName, Deserialize, Deserializer, McpBlock, Name, Origin, RawJson, Serialize, fmt,
};

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

    pub(super) const fn mask(self) -> u16 {
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
    pub(super) bits: u16,
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

    /// Returns the union of two request sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
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

impl fmt::Display for Site {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}:{}",
            self.path.display(),
            self.line,
            self.col
        )
    }
}
/// A rule file selected by the host for one extension generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleFile {
    /// The extension that owns the file.
    pub plugin: Name,
    /// The extension's source class.
    pub origin: Origin,
    /// The source path reported by the host.
    pub path: Box<str>,
    /// The complete file contents.
    pub bytes: Arc<[u8]>,
}
/// An embedded plugin source tree passed across the dal product boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSource {
    /// The plugin's checked name.
    pub name: Box<str>,
    /// Files keyed by their plugin-relative paths.
    pub files: std::collections::BTreeMap<std::path::PathBuf, &'static [u8]>,
}

impl PluginSource {
    /// Builds a source tree from statically embedded files.
    #[must_use]
    pub fn embedded(
        name: impl Into<Box<str>>,
        files: std::collections::BTreeMap<std::path::PathBuf, &'static [u8]>,
    ) -> Self {
        Self {
            name: name.into(),
            files,
        }
    }
}

/// A shared tool declaration with its schema retained as raw JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    /// The tool's registered name.
    #[serde(deserialize_with = "super::names::deserialize_tool_name")]
    pub name: Name,
    /// A human-readable description of the tool.
    pub description: Box<str>,
    /// The input schema, preserved as raw JSON.
    pub parameters: RawJson,
    /// Optional constrained grammar accepted by capable model routes.
    pub grammar: Option<Box<str>>,
}

/// Metadata for a host command exposed by an extension.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
    /// The command's registered name.
    pub name: CommandName,
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
    /// The MCP servers the skill declares in its front matter, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpBlock>,
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
    /// A command identifier does not match any accepted command grammar.
    #[error(
        "invalid command name \"{name}\"; names must match [a-z][a-z0-9_-]{{0,63}}, skill:[a-z0-9-]+, or <plugin>:<command>"
    )]
    InvalidCommandName {
        /// The rejected command identifier.
        name: Box<str>,
    },
    /// A synthetic model identifier does not match its namespace/name grammar.
    #[error("invalid synthetic model id `{id}`: expected `[a-z0-9-]+/[a-z0-9._-]+`")]
    InvalidModelId {
        /// The rejected model identifier.
        id: Box<str>,
    },
    /// A declared version string does not conform to `SemVer`.
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
    #[error("extension \"{ext}\" may register only one status kind")]
    DuplicateStatusKind {
        /// The extension that tried to register twice.
        ext: Name,
    },
    /// A skill declares MCP servers but its extension does not inject MCP.
    #[error(
        "skill \"{skill}\" in extension \"{extension}\" declares MCP servers but does not inject \"mcp\""
    )]
    McpSkillNotInjected {
        /// The skill requiring the MCP capability.
        skill: Name,
        /// The extension that owns the skill.
        extension: Name,
    },
    /// A tool's declaring extension, skill, or server does not exist.
    #[error(
        "tool \"{tool}\" has an invalid MCP declaration for extension \"{extension}\": {reason}"
    )]
    InvalidMcpTool {
        /// The mapped tool name.
        tool: Box<str>,
        /// The extension the tool says declares its server.
        extension: Name,
        /// The missing or malformed declaration component.
        reason: &'static str,
    },
    /// A rule declaration failed one of its local value checks.
    #[error("rule \"{name}\" {reason}")]
    InvalidRule {
        /// The rule name.
        name: Name,
        /// The validation failure in concise user-facing form.
        reason: Box<str>,
    },
    /// A scripted export is not a tool of its own plugin under its
    /// `<plugin>__<local>` wire name (R02).
    #[error("export \"{id}\" must be a tool of its own plugin registered as \"{wire}\"")]
    InvalidExport {
        /// The export's `uses` spelling.
        id: Box<str>,
        /// The required wire name.
        wire: Box<str>,
    },
    /// A doc scheme takes a host-owned namespace.
    #[error("{site}: doc scheme '{scheme}' is reserved")]
    DocSchemeReserved {
        /// The reserved scheme name.
        scheme: Box<str>,
        /// The doc call site.
        site: Site,
    },
    /// A doc scheme is outside the scheme grammar.
    #[error("{site}: doc scheme '{scheme}' is not a valid scheme name")]
    InvalidDocScheme {
        /// The rejected scheme name.
        scheme: Box<str>,
        /// The doc call site.
        site: Site,
    },
    /// A doc path is outside the page grammar.
    #[error("{site}: doc path '{path}' is not a page name")]
    InvalidDocPage {
        /// The rejected page path.
        path: Box<str>,
        /// The doc call site.
        site: Site,
    },
    /// A doc URI is registered twice in one generation.
    #[error("doc uri '{uri}' is already registered")]
    DuplicateDocUri {
        /// The duplicated URI.
        uri: Box<str>,
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

pub(super) fn valid_identifiers(value: &str, reject_numeric_leading_zero: bool) -> bool {
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
pub(super) struct ToolSchemaType {
    #[serde(default, rename = "type", deserialize_with = "deserialize_schema_type")]
    pub(super) kind: Option<Box<str>>,
}

pub(super) fn deserialize_schema_type<'de, D>(deserializer: D) -> Result<Option<Box<str>>, D::Error>
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
