//! MCP server declarations a skill carries, and their value rules.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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

/// One declared MCP server: a child process or a streamable-HTTP endpoint.
///
/// The wire form is one object: `command` with optional `env`, or `url`.
/// Decoding rejects unknown keys, both transports at once, neither, and
/// `env` next to `url`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "ServerWire", into = "ServerWire")]
#[cfg_attr(feature = "schema", schemars(with = "ServerWire"))]
pub enum McpServerDecl {
    /// A child process launched from an argv, without a shell.
    Stdio {
        /// The argv; `command[0]` resolves through `PATH`.
        command: Vec<Box<str>>,
        /// Literal environment entries added to the child; never expanded.
        env: BTreeMap<Box<str>, Box<str>>,
    },
    /// A streamable-HTTP endpoint.
    Http {
        /// The absolute `https` URL.
        url: Box<str>,
    },
}

/// The serialized object of an [`McpServerDecl`].
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerWire {
    /// The argv of a stdio server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<Box<str>>>,
    /// The literal environment of a stdio server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<Box<str>, Box<str>>>,
    /// The URL of an HTTP server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<Box<str>>,
}

/// A server object that names no transport, both, or `env` without `command`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ServerShapeError {
    /// Neither `command` nor `url` is present.
    #[error("a server needs exactly one of \"command\" or \"url\"; found neither")]
    Neither,
    /// Both `command` and `url` are present.
    #[error("a server needs exactly one of \"command\" or \"url\"; found both")]
    Both,
    /// `env` is present next to `url`.
    #[error("\"env\" belongs to a \"command\" server; it is not valid next to \"url\"")]
    EnvWithUrl,
}

impl TryFrom<ServerWire> for McpServerDecl {
    type Error = ServerShapeError;

    fn try_from(wire: ServerWire) -> Result<Self, Self::Error> {
        match wire {
            ServerWire {
                command: Some(command),
                env,
                url: None,
            } => Ok(Self::Stdio {
                command,
                env: env.unwrap_or_default(),
            }),
            ServerWire {
                command: None,
                env: None,
                url: Some(url),
            } => Ok(Self::Http { url }),
            ServerWire {
                command: None,
                env: Some(_),
                url: Some(_),
            } => Err(ServerShapeError::EnvWithUrl),
            ServerWire {
                command: Some(_),
                url: Some(_),
                ..
            } => Err(ServerShapeError::Both),
            ServerWire {
                command: None,
                url: None,
                ..
            } => Err(ServerShapeError::Neither),
        }
    }
}

impl From<McpServerDecl> for ServerWire {
    fn from(decl: McpServerDecl) -> Self {
        match decl {
            McpServerDecl::Stdio { command, env } => Self {
                command: Some(command),
                env: (!env.is_empty()).then_some(env),
                url: None,
            },
            McpServerDecl::Http { url } => Self {
                command: None,
                env: None,
                url: Some(url),
            },
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
    /// The `command` array is empty.
    #[error("mcp server \"{server}\": \"command\" must be a non-empty array")]
    EmptyCommand {
        /// The server whose command is empty.
        server: Box<str>,
    },
    /// `command[0]` is empty, so nothing resolves through `PATH`.
    #[error("mcp server \"{server}\": \"command\" must start with a program name")]
    EmptyProgram {
        /// The server whose program name is empty.
        server: Box<str>,
    },
    /// The URL is not an absolute `https` URL with a host.
    #[error("mcp server \"{server}\": \"url\" must be an absolute https URL")]
    Url {
        /// The server whose URL is rejected.
        server: Box<str>,
    },
    /// An `env` key does not match `[A-Za-z_][A-Za-z0-9_]*`.
    #[error("mcp server \"{server}\": env key \"{key}\" is invalid; use [A-Za-z_][A-Za-z0-9_]*")]
    EnvKey {
        /// The server whose environment holds the key.
        server: Box<str>,
        /// The rejected environment key.
        key: Box<str>,
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
            | Self::EnvKey { server, .. } => server,
        }
    }
}

/// Checks every entry of `block` against the value rules.
///
/// Rules: a server name is 1 to 64 characters of `A-Z a-z 0-9 _ -`;
/// `command` is a non-empty argv whose first element is non-empty; `url` is
/// an absolute `https` URL with a host; `env` keys match
/// `[A-Za-z_][A-Za-z0-9_]*`. Values are literal, never expanded. The
/// exactly-one-transport rule holds by construction of [`McpServerDecl`].
///
/// # Errors
///
/// Returns one [`McpBlockError`] per bad entry, in server-name order.
pub fn validate_block(block: &McpBlock) -> Result<(), Vec<McpBlockError>> {
    let mut errors = Vec::new();
    for (server, decl) in &block.servers {
        if !valid_server_name(server) {
            errors.push(McpBlockError::ServerName {
                server: server.clone(),
            });
        }
        match decl {
            McpServerDecl::Stdio { command, env } => {
                check_stdio(server, command, env, &mut errors);
            }
            McpServerDecl::Http { url } => {
                if !valid_https_url(url) {
                    errors.push(McpBlockError::Url {
                        server: server.clone(),
                    });
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn check_stdio(
    server: &str,
    command: &[Box<str>],
    env: &BTreeMap<Box<str>, Box<str>>,
    errors: &mut Vec<McpBlockError>,
) {
    match command.first() {
        None => errors.push(McpBlockError::EmptyCommand {
            server: server.into(),
        }),
        Some(program) if program.is_empty() => errors.push(McpBlockError::EmptyProgram {
            server: server.into(),
        }),
        Some(_) => {}
    }
    for key in env.keys().filter(|key| !valid_env_key(key)) {
        errors.push(McpBlockError::EnvKey {
            server: server.into(),
            key: key.clone(),
        });
    }
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

fn valid_https_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https")
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = if host_port.starts_with('[') {
        host_port.split_once(']').map_or("", |(inner, _)| inner)
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    !host.is_empty() && host != "["
}
