//! MCP server declarations a skill carries, and their value rules.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::BTreeMap;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use url::{SyntaxViolation, Url};

use super::Name;

const MAX_SERVER_NAME_CHARS: usize = 64;

/// The MCP servers one skill declares, keyed by server name.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct McpBlock {
    /// The declared servers, ordered by name.
    pub servers: BTreeMap<Box<str>, McpServerDecl>,
}

/// A validated launch recipe for a stdio MCP server.
///
/// The recipe owns its validated selector and arguments.  Package forms lower
/// to the runner argv used by the existing stdio transport; arbitrary command
/// argv remains opaque and is never interpreted as a package invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpLaunch {
    kind: LaunchWire,
}

/// The tagged serialized launch recipe.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum LaunchWire {
    /// An opaque arbitrary command argv.
    Command { argv: Vec<Box<str>> },
    /// An npx package selector.
    Npx {
        package: Box<str>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "non_null"
        )]
        version: Option<Box<str>>,
        #[serde(default, deserialize_with = "non_null_vec")]
        args: Vec<Box<str>>,
    },
    /// An uvx package and executable selector.
    Uvx {
        package: Box<str>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "non_null"
        )]
        version: Option<Box<str>>,
        executable: Box<str>,
        #[serde(default, deserialize_with = "non_null_vec")]
        args: Vec<Box<str>>,
    },
}

impl McpLaunch {
    /// Builds an opaque command launch while preserving every argv token.
    ///
    /// # Errors
    /// Returns [`McpTargetError`] when argv is empty, its program is empty, or
    /// any token contains a NUL byte.
    pub fn command(argv: Vec<Box<str>>) -> Result<Self, McpTargetError> {
        Self::from_wire(LaunchWire::Command { argv })
    }

    /// Builds the canonical `npx --yes -- package[@version] args...` launch.
    ///
    /// # Errors
    /// Returns [`McpTargetError`] for an invalid package, version, or NUL
    /// argument.
    pub fn npx(
        package: Box<str>,
        version: Option<Box<str>>,
        args: Vec<Box<str>>,
    ) -> Result<Self, McpTargetError> {
        Self::from_wire(LaunchWire::Npx {
            package,
            version,
            args,
        })
    }

    /// Builds the canonical `uvx --from package[==version] executable args...`
    /// launch.
    ///
    /// # Errors
    /// Returns [`McpTargetError`] for an invalid package, version, executable,
    /// or NUL argument.
    pub fn uvx(
        package: Box<str>,
        version: Option<Box<str>>,
        executable: Box<str>,
        args: Vec<Box<str>>,
    ) -> Result<Self, McpTargetError> {
        Self::from_wire(LaunchWire::Uvx {
            package,
            version,
            executable,
            args,
        })
    }

    /// Returns the argv consumed by the existing stdio transport.
    ///
    /// Opaque commands borrow their validated original argv. Package recipes
    /// lower to one owned argv for the actual launch.
    #[must_use]
    pub fn execution_argv(&self) -> Cow<'_, [Box<str>]> {
        match &self.kind {
            LaunchWire::Command { argv } => Cow::Borrowed(argv),
            LaunchWire::Npx {
                package,
                version,
                args,
            } => {
                let mut argv = Vec::with_capacity(4 + args.len());
                argv.push("npx".into());
                argv.push("--yes".into());
                argv.push("--".into());
                argv.push(package_selector(package, version.as_deref(), "@").into());
                argv.extend(args.iter().cloned());
                Cow::Owned(argv)
            }
            LaunchWire::Uvx {
                package,
                version,
                executable,
                args,
            } => {
                let mut argv = Vec::with_capacity(4 + args.len());
                argv.push("uvx".into());
                argv.push("--from".into());
                argv.push(package_selector(package, version.as_deref(), "==").into());
                argv.push(executable.clone());
                argv.extend(args.iter().cloned());
                Cow::Owned(argv)
            }
        }
    }

    fn from_wire(wire: LaunchWire) -> Result<Self, McpTargetError> {
        match &wire {
            LaunchWire::Command { argv } => validate_command(argv)?,
            LaunchWire::Npx {
                package,
                version,
                args,
            } => {
                validate_package(package, PackageKind::Npm)?;
                if let Some(version) = version {
                    validate_version(version)?;
                }
                validate_args(args)?;
            }
            LaunchWire::Uvx {
                package,
                version,
                executable,
                args,
            } => {
                validate_package(package, PackageKind::Python)?;
                if let Some(version) = version {
                    validate_version(version)?;
                }
                validate_executable(executable)?;
                validate_args(args)?;
            }
        }
        Ok(Self { kind: wire })
    }
}

/// A single parsed and validated HTTP endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpEndpoint {
    url: Url,
}

impl McpEndpoint {
    /// Parses one public MCP endpoint using the shared URL parser.
    ///
    /// # Errors
    /// Rejects non-HTTPS URLs, missing hosts, userinfo, fragments, whitespace,
    /// controls, and backslashes before approval.
    pub fn parse(raw: &str) -> Result<Self, McpTargetError> {
        if raw.chars().any(|character| {
            character.is_whitespace() || character.is_control() || character == '\\'
        }) {
            return Err(McpTargetError::InvalidEndpoint);
        }
        let credentials = Cell::new(false);
        let on_violation = |violation| {
            if matches!(
                violation,
                SyntaxViolation::EmbeddedCredentials | SyntaxViolation::UnencodedAtSign
            ) {
                credentials.set(true);
            }
        };
        let url = Url::options()
            .syntax_violation_callback(Some(&on_violation))
            .parse(raw)
            .map_err(|_| McpTargetError::InvalidEndpoint)?;
        if credentials.get()
            || !url.scheme().eq_ignore_ascii_case("https")
            || url.host_str().is_none_or(str::is_empty)
            || url.fragment().is_some()
        {
            return Err(McpTargetError::InvalidEndpoint);
        }
        Ok(Self { url })
    }

    /// Returns the sole parsed URL used by the HTTP transport.
    #[must_use]
    pub const fn as_url(&self) -> &Url {
        &self.url
    }
}

/// A target validation failure. Error text never includes the rejected value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum McpTargetError {
    /// The server object selected no transport or selected incompatible fields.
    #[error("{0}")]
    Shape(ServerShapeError),
    /// A command argv has no elements.
    #[error("MCP command argv must be a non-empty array")]
    EmptyCommand,
    /// The first command token is empty.
    #[error("MCP command argv must start with a program name")]
    EmptyProgram,
    /// A command, package argument, or executable contains NUL.
    #[error("MCP launch values must not contain NUL")]
    NulArgument,
    /// An environment value contains NUL.
    #[error("MCP environment values must not contain NUL")]
    NulEnvironment,
    /// A package token does not match its bounded public grammar.
    #[error("MCP package name is invalid")]
    InvalidPackage,
    /// A version token does not match its bounded public grammar.
    #[error("MCP package version is invalid")]
    InvalidVersion,
    /// A uvx executable token is not a simple command name.
    #[error("MCP uvx executable is invalid")]
    InvalidExecutable,
    /// An endpoint is not an approved HTTPS URL.
    #[error("MCP endpoint is invalid")]
    InvalidEndpoint,
}

/// One declared MCP server: a child process or a streamable-HTTP endpoint.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "ServerWire")]
#[cfg_attr(feature = "schema", schemars(with = "ServerWire"))]
pub enum McpServerDecl {
    /// A child process launched from one validated recipe, without a shell.
    Stdio {
        /// The validated launch recipe.
        launch: McpLaunch,
        /// Literal environment entries added to the child; never expanded.
        env: BTreeMap<Box<str>, Box<str>>,
    },
    /// A streamable-HTTP endpoint.
    Http {
        /// The one parsed and validated endpoint URL.
        endpoint: McpEndpoint,
    },
}
impl Serialize for McpServerDecl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Stdio { launch, env } => {
                let mut state = serializer
                    .serialize_struct("ServerWire", if env.is_empty() { 1 } else { 2 })?;
                state.serialize_field("launch", &launch.kind)?;
                if !env.is_empty() {
                    state.serialize_field("env", env)?;
                }
                state.end()
            }
            Self::Http { endpoint } => {
                let mut state = serializer.serialize_struct("ServerWire", 1)?;
                state.serialize_field("url", endpoint.url.as_str())?;
                state.end()
            }
        }
    }
}

impl McpServerDecl {
    /// Returns a public target identity with all secret-capable operands hidden.
    #[must_use]
    pub fn redacted_target(&self) -> String {
        match self {
            Self::Stdio { launch, .. } => match &launch.kind {
                LaunchWire::Command { .. } => {
                    "stdio: opaque command; executable and arguments <redacted>".to_owned()
                }
                LaunchWire::Npx {
                    package, version, ..
                } => format!(
                    "stdio: npx package {}; arguments <redacted>",
                    public_package_identity(package, version.as_deref(), "@")
                ),
                LaunchWire::Uvx {
                    package,
                    version,
                    executable,
                    ..
                } => format!(
                    "stdio: uvx package {}; executable {}; arguments <redacted>",
                    public_package_identity(package, version.as_deref(), "=="),
                    executable
                ),
            },
            Self::Http { endpoint } => {
                format!("{}://{}", endpoint.url.scheme(), endpoint.url.authority())
            }
        }
    }
}

/// The serialized object of an [`McpServerDecl`].
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerWire {
    /// The tagged launch recipe of a stdio server.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null"
    )]
    pub launch: Option<LaunchWire>,
    /// The literal environment of a stdio server.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null"
    )]
    pub env: Option<BTreeMap<Box<str>, Box<str>>>,
    /// The URL of an HTTP server.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null"
    )]
    pub url: Option<Box<str>>,
    /// The removed legacy selector; any presence fails with a migration error.
    #[serde(default, deserialize_with = "reject_command", skip_serializing)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub command: (),
}

fn non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
fn non_null_vec<'de, D>(deserializer: D) -> Result<Vec<Box<str>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Vec<Box<str>>>::deserialize(deserializer)?
        .ok_or_else(|| serde::de::Error::custom("null is not allowed"))
}
fn reject_command<'de, D>(deserializer: D) -> Result<(), D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::de::IgnoredAny::deserialize(deserializer)?;
    Err(serde::de::Error::custom(
        "\"command\" was renamed to \"launch\"; move the argv list to \"launch\" with \"kind: command\" and \"argv\"",
    ))
}

/// A server object that names no transport, both, or `env` without `launch`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ServerShapeError {
    /// Neither `launch` nor `url` is present.
    #[error("a server needs exactly one of \"launch\" or \"url\"; found neither")]
    Neither,
    /// Both `launch` and `url` are present.
    #[error("a server needs exactly one of \"launch\" or \"url\"; found both")]
    Both,
    /// `env` is present next to `url`.
    #[error("\"env\" belongs to a \"launch\" server; it is not valid next to \"url\"")]
    EnvWithUrl,
}

impl TryFrom<ServerWire> for McpServerDecl {
    type Error = McpTargetError;

    fn try_from(wire: ServerWire) -> Result<Self, Self::Error> {
        match wire {
            ServerWire {
                launch: Some(launch),
                env,
                url: None,
                ..
            } => {
                let env = env.unwrap_or_default();
                if env.values().any(|value| value.contains('\0')) {
                    return Err(McpTargetError::NulEnvironment);
                }
                Ok(Self::Stdio {
                    launch: McpLaunch::from_wire(launch)?,
                    env,
                })
            }
            ServerWire {
                launch: None,
                env: None,
                url: Some(url),
                ..
            } => Ok(Self::Http {
                endpoint: McpEndpoint::parse(&url)?,
            }),
            ServerWire {
                launch: None,
                env: Some(_),
                url: Some(_),
                ..
            } => Err(McpTargetError::Shape(ServerShapeError::EnvWithUrl)),
            ServerWire {
                launch: Some(_),
                url: Some(_),
                ..
            } => Err(McpTargetError::Shape(ServerShapeError::Both)),
            ServerWire {
                launch: None,
                url: None,
                ..
            } => Err(McpTargetError::Shape(ServerShapeError::Neither)),
        }
    }
}

/// One declared block of the current generation, with its owners.
///
/// `Services::mcp_declarations` returns one per skill that carries a block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct McpDeclaration {
    /// The plugin that registered the skill.
    pub plugin: Name,
    /// The skill that declares the block.
    pub skill: Name,
    /// The declared servers.
    pub block: McpBlock,
}

/// One value rule an [`McpBlock`] entry breaks.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum McpBlockError {
    /// The server name is empty, longer than 64 characters, or has a
    /// character outside `A-Z a-z 0-9 _ -`.
    #[error(
        "mcp server name \"{server}\" is invalid; use 1 to 64 characters of A-Z, a-z, 0-9, _ and -"
    )]
    ServerName {
        /// The rejected server name.
        server: Box<str>,
    },
    /// The `command` recipe is empty.
    #[error("mcp server \"{server}\": \"argv\" must be a non-empty array")]
    EmptyCommand {
        /// The server whose command is empty.
        server: Box<str>,
    },
    /// The first command token is empty.
    #[error("mcp server \"{server}\": \"argv\" must start with a program name")]
    EmptyProgram {
        /// The server whose program name is empty.
        server: Box<str>,
    },
    /// The endpoint is not a validated HTTPS URL.
    #[error("mcp server \"{server}\": \"url\" must be an absolute https URL")]
    Url {
        /// The server whose URL is rejected.
        server: Box<str>,
    },
    /// A launch recipe violates its value rules.
    #[error("mcp server \"{server}\": {error}")]
    Target {
        /// The server whose launch is rejected.
        server: Box<str>,
        /// The safe target validation error.
        error: McpTargetError,
    },
    /// An `env` key does not match `[A-Za-z_][A-Za-z0-9_]*`.
    #[error("mcp server \"{server}\": env key \"{key}\" is invalid; use [A-Za-z_][A-Za-z0-9_]*")]
    EnvKey {
        /// The server whose environment holds the key.
        server: Box<str>,
        /// The rejected environment key.
        key: Box<str>,
    },
    /// An environment value contains NUL.
    #[error("mcp server \"{server}\": environment values must not contain NUL")]
    EnvValue {
        /// The server whose environment is rejected.
        server: Box<str>,
    },
}

impl McpBlockError {
    /// The server the entry belongs to.
    #[must_use]
    pub fn server(&self) -> &str {
        match self {
            Self::ServerName { server }
            | Self::EmptyCommand { server }
            | Self::EmptyProgram { server }
            | Self::Url { server }
            | Self::Target { server, .. }
            | Self::EnvKey { server, .. }
            | Self::EnvValue { server } => server,
        }
    }
}

/// Checks every entry of `block` against the value rules.
///
/// Rules: a server name is 1 to 64 characters of `A-Z a-z 0-9 _ -`; launch
/// recipes are validated constructors; HTTP endpoints are parsed HTTPS URLs;
/// `env` keys match `[A-Za-z_][A-Za-z0-9_]*` and values are literal, never
/// expanded. The exactly-one-transport rule holds by construction of
/// [`McpServerDecl`].
///
/// # Errors
/// Returns one [`McpBlockError`] per bad entry, in server-name order.
pub fn validate_block(block: &McpBlock) -> Result<(), Vec<McpBlockError>> {
    let mut errors = Vec::new();
    for (server, decl) in &block.servers {
        if !valid_server_name(server) {
            errors.push(McpBlockError::ServerName {
                server: server.clone(),
            });
        }
        if let McpServerDecl::Stdio { env, .. } = decl {
            check_env(server, env, &mut errors);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn check_env(server: &str, env: &BTreeMap<Box<str>, Box<str>>, errors: &mut Vec<McpBlockError>) {
    for key in env.keys().filter(|key| !valid_env_key(key)) {
        errors.push(McpBlockError::EnvKey {
            server: server.into(),
            key: key.clone(),
        });
    }
    if env.values().any(|value| value.contains('\0')) {
        errors.push(McpBlockError::EnvValue {
            server: server.into(),
        });
    }
}

fn validate_command(argv: &[Box<str>]) -> Result<(), McpTargetError> {
    match argv.first() {
        None => Err(McpTargetError::EmptyCommand),
        Some(program) if program.is_empty() => Err(McpTargetError::EmptyProgram),
        Some(_) => validate_args(argv),
    }
}

fn validate_args(args: &[Box<str>]) -> Result<(), McpTargetError> {
    if args.iter().any(|value| value.contains('\0')) {
        Err(McpTargetError::NulArgument)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum PackageKind {
    Npm,
    Python,
}

fn validate_package(package: &str, kind: PackageKind) -> Result<(), McpTargetError> {
    let valid = match kind {
        PackageKind::Npm => valid_npm_package(package),
        PackageKind::Python => valid_python_package(package),
    };
    if valid {
        Ok(())
    } else {
        Err(McpTargetError::InvalidPackage)
    }
}

fn valid_npm_package(package: &str) -> bool {
    let valid_component = |component: &str| {
        !component.is_empty()
            && component != "."
            && component != ".."
            && component.chars().all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || matches!(character, '.' | '_' | '-')
            })
            && component.chars().next().is_some_and(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit()
            })
    };
    if let Some(scoped) = package.strip_prefix('@') {
        let Some((scope, name)) = scoped.split_once('/') else {
            return false;
        };
        valid_component(scope) && valid_component(name) && scoped.matches('/').count() == 1
    } else {
        valid_component(package)
    }
}

fn valid_python_package(package: &str) -> bool {
    let mut chars = package.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphanumeric())
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
        && package
            .chars()
            .last()
            .is_some_and(|character| character.is_ascii_alphanumeric())
}

fn validate_version(version: &str) -> Result<(), McpTargetError> {
    let (base, build) = version
        .split_once('+')
        .map_or((version, None), |(base, build)| (base, Some(build)));
    let (release, pre) = base
        .split_once('-')
        .map_or((base, None), |(release, pre)| (release, Some(pre)));
    if release.is_empty()
        || release
            .split('.')
            .any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()))
        || pre.is_some_and(|value| {
            value.is_empty()
                || !value
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
                || !value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        })
        || build.is_some_and(|value| {
            value.is_empty()
                || !value
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
                || !value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        })
    {
        Err(McpTargetError::InvalidVersion)
    } else {
        Ok(())
    }
}

fn validate_executable(executable: &str) -> Result<(), McpTargetError> {
    let mut chars = executable.chars();
    let Some(first) = chars.next() else {
        return Err(McpTargetError::InvalidExecutable);
    };
    if !(first.is_ascii_alphanumeric() || first == '_')
        || executable == "."
        || executable == ".."
        || !chars.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
    {
        Err(McpTargetError::InvalidExecutable)
    } else {
        Ok(())
    }
}

fn package_selector(package: &str, version: Option<&str>, separator: &str) -> String {
    version.map_or_else(
        || package.to_owned(),
        |version| format!("{package}{separator}{version}"),
    )
}

fn public_package_identity(package: &str, version: Option<&str>, separator: &str) -> String {
    version.map_or_else(
        || format!("{package} (unpinned)"),
        |version| format!("{package}{separator}{version}"),
    )
}

fn valid_server_name(name: &str) -> bool {
    let count = name.chars().count();
    (1..=MAX_SERVER_NAME_CHARS).contains(&count)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_launch_lowering_preserves_opaque_args() {
        let args = vec!["--token=secret".into(), "".into(), "$(touch x)".into()];
        let launch =
            McpLaunch::npx("@scope/server".into(), Some("1.2.3".into()), args.clone()).unwrap();
        assert_eq!(
            launch.execution_argv().as_ref(),
            &[
                "npx".into(),
                "--yes".into(),
                "--".into(),
                "@scope/server@1.2.3".into(),
                "--token=secret".into(),
                "".into(),
                "$(touch x)".into(),
            ]
        );
        let command =
            McpLaunch::command(vec!["/bin/sh".into(), "-c".into(), "echo $TOKEN".into()]).unwrap();
        assert!(matches!(command.execution_argv(), Cow::Borrowed(_)));
        let uvx = McpLaunch::uvx(
            "mcp-route-proof".into(),
            Some("1.0.0".into()),
            "proof-cmd".into(),
            vec!["--token=secret".into(), "".into()],
        )
        .unwrap();
        assert_eq!(
            uvx.execution_argv().as_ref(),
            &[
                "uvx".into(),
                "--from".into(),
                "mcp-route-proof==1.0.0".into(),
                "proof-cmd".into(),
                "--token=secret".into(),
                "".into(),
            ]
        );
        let decl = McpServerDecl::Stdio {
            launch,
            env: [("API_TOKEN".into(), "secret".into())].into(),
        };
        let preview = decl.redacted_target();
        assert!(preview.contains("@scope/server@1.2.3"));
        assert!(!preview.contains("secret"));
    }

    #[test]
    fn constructors_reject_bad_identity_and_nul_args() {
        let unpinned = McpLaunch::npx("server".into(), None, Vec::new()).unwrap();
        assert_eq!(
            unpinned.execution_argv().as_ref(),
            &["npx".into(), "--yes".into(), "--".into(), "server".into()]
        );
        let preview = McpServerDecl::Stdio {
            launch: unpinned,
            env: BTreeMap::new(),
        }
        .redacted_target();
        assert!(preview.contains("server (unpinned)"));
        assert!(McpLaunch::npx("server".into(), Some("1.*".into()), Vec::new()).is_err());
        assert!(McpLaunch::uvx("server".into(), None, "bin/name".into(), Vec::new()).is_err());
        assert!(McpLaunch::command(vec!["prog\0".into()]).is_err());
        assert!(McpLaunch::npx("server".into(), None, vec!["arg\0".into()]).is_err());
    }

    #[test]
    fn endpoint_rejects_credentials_fragments_and_raw_separators() {
        for raw in [
            "https://user@example.com/mcp",
            "https://example.com/mcp#fragment",
            "https://example.com/mcp\\path",
            "https://example.com/mcp path",
        ] {
            assert!(McpEndpoint::parse(raw).is_err(), "{raw}");
        }
        let endpoint = McpEndpoint::parse("HTTPS://example.com/private?token=secret").unwrap();
        assert_eq!(endpoint.as_url().scheme(), "https");
        let decl = McpServerDecl::Http { endpoint };
        let preview = decl.redacted_target();
        assert_eq!(preview, "https://example.com");
        assert!(!preview.contains("private"));
        assert!(!preview.contains("secret"));
    }

    #[test]
    fn wire_shape_and_nulls_are_rejected_without_aliases() {
        for raw in [
            r#"{"launch":{"kind":"command","argv":["x"]},"command":["x"]}"#,
            r#"{"launch":{"kind":"command","argv":null}}"#,
            r#"{"launch":{"kind":"npx","package":"x","version":null}}"#,
            r#"{"launch":{"kind":"npx","package":"x","args":null}}"#,
            r#"{"launch":{"kind":"npx","package":"server","unknown":1}}"#,
            r#"{"launch":{"kind":"npx","package":"server","package":"other"}}"#,
        ] {
            assert!(sonic_rs::from_str::<McpServerDecl>(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn old_command_shape_is_rejected_with_launch_migration() {
        for raw in [
            r#"{"command":["x"]}"#,
            r#"{"launch":{"kind":"command","argv":["x"]},"command":["x"]}"#,
        ] {
            let error = sonic_rs::from_str::<McpServerDecl>(raw)
                .expect_err("the old command shape must stay rejected");
            let message = error.to_string();
            assert!(
                message.contains("renamed")
                    && message.contains("launch")
                    && message.contains("command"),
                "the rejection must name launch as the replacement: {raw} -> {message}"
            );
        }
        for raw in [
            r#"{"servers":{"a":{"command":["x"]}}}"#,
            r#"{"servers":{"a":{"launch":{"kind":"command","argv":["x"]},"command":["x"]}}}"#,
        ] {
            let error = sonic_rs::from_str::<McpBlock>(raw)
                .expect_err("the old command shape must stay rejected");
            let message = error.to_string();
            assert!(
                message.contains("renamed")
                    && message.contains("launch")
                    && message.contains("command"),
                "the rejection must name launch as the replacement: {raw} -> {message}"
            );
        }
    }

    #[test]
    fn unpinned_package_declarations_survive_serialization() {
        for launch in [
            McpLaunch::npx("server".into(), None, vec!["private-argument".into()]).unwrap(),
            McpLaunch::uvx("server".into(), None, "server-bin".into(), Vec::new()).unwrap(),
        ] {
            let declaration = McpServerDecl::Stdio {
                launch,
                env: BTreeMap::new(),
            };
            let encoded = sonic_rs::to_string(&declaration).unwrap();
            let restored: McpServerDecl = sonic_rs::from_str(&encoded)
                .expect("a validated unpinned declaration must remain readable");
            assert_eq!(restored, declaration);
        }
    }

    #[test]
    fn endpoint_rejects_userinfo_even_when_empty() {
        for raw in [
            "https://@example.com/mcp",
            "https://:@example.com/mcp",
            "https://user:pass@example.com/mcp",
        ] {
            assert!(
                McpEndpoint::parse(raw).is_err(),
                "userinfo must be rejected before URL normalization: {raw}"
            );
        }
    }

    #[test]
    fn malformed_operand_errors_remain_type_errors() {
        for raw in [
            r#"{"launch":{"kind":"command","argv":"MCP_ARG_SECRET"}}"#,
            r#"{"launch":{"kind":"npx","package":"server","args":"MCP_ARG_SECRET"}}"#,
            r#"{"launch":{"kind":"command","argv":["program"]},"env":"MCP_ARG_SECRET"}"#,
            r#"{"url":["MCP_ARG_SECRET"]}"#,
        ] {
            let error = sonic_rs::from_str::<McpServerDecl>(raw)
                .expect_err("malformed operands must be rejected");
            assert!(
                error.is_unmatched_type(),
                "malformed operands must retain a type-category diagnostic: {error}"
            );
        }
    }
}
