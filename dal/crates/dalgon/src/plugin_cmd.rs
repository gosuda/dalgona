//! Commands that inspect and persist configured plugin grants.

use std::{io::Write, process::ExitCode};

use dal_agent::ext::Extension;
use dal_agent::{GrantKey, GrantStore, GrantStoreError};
use dal_core::{ClientId, Name, Origin, ServiceSet};
use thiserror::Error as ThisError;

/// A request to inspect or update configured plugin grants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginCommand {
    /// Persist the exact declared capability set for one configured plugin.
    Grant {
        /// The configured plugin name.
        name: Name,
    },
    /// Remove every persistent grant that names one extension.
    Revoke {
        /// The extension name whose grants are removed.
        name: Name,
    },
    /// List configured plugins and their exact persistent grants.
    List,
}

/// A plugin grant status or the command that produced it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrantStatus {
    /// The exact declared set is already persisted.
    Granted,
    /// The exact declared set is not persisted.
    NotGranted,
    /// The plugin declares no grantable service.
    NotRequired,
}

/// A snapshot of one configured user or bundled plugin declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredPlugin {
    /// The configured plugin name.
    pub name: Name,
    /// Whether this was a user or bundled plugin declaration.
    pub origin: Origin,
    /// The services declared by the plugin, with `ask` removed.
    pub capabilities: ServiceSet,
}

impl ConfiguredPlugin {
    /// Creates one declaration from a loaded extension record.
    ///
    /// Built-in extensions are not grantable and have no declared services here.
    #[must_use]
    pub fn from_extension(extension: &Extension) -> Option<Self> {
        // Builtins carry no grantable declaration, so only User/Bundled yield a snapshot.
        match extension.origin() {
            Origin::User | Origin::Bundled => Some(Self {
                name: extension.name().parse().ok()?,
                origin: extension.origin(),
                capabilities: extension.inject().capabilities(),
            }),
            _ => None,
        }
    }

    /// Returns the current grant status for this exact declared service set.
    ///
    /// # Errors
    /// Returns the backing store's typed error when the grant cannot be read.
    pub async fn status(&self, grants: &GrantStore) -> Result<GrantStatus, GrantStoreError> {
        if self.capabilities.is_empty() {
            return Ok(GrantStatus::NotRequired);
        }
        let key = GrantKey {
            extension: self.name.clone(),
            origin: self.origin,
            services: self.capabilities,
        };
        grants.contains(&key).await.map(|granted| {
            if granted {
                GrantStatus::Granted
            } else {
                GrantStatus::NotGranted
            }
        })
    }
}

/// A plugin command could not be completed.
#[derive(Debug, ThisError)]
pub enum PluginCommandError {
    /// The named plugin has no effective configured declaration.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The backing grant store rejected the request with a typed error.
    #[error(transparent)]
    Store(#[from] GrantStoreError),
    /// The requested grant names a builtin extension.
    #[error("builtin extensions need no grant")]
    Builtin,
}

/// Runs one plugin grant, revoke, or list command.
///
/// `plugins` carries the loaded effective declarations in any order. This
/// module re-sorts them by name byte order before it responds.
///
/// # Errors
/// Returns a stream-I/O error or the backing grant store's typed error. A
/// request for an unconfigured plugin fails without changing the store.
#[expect(
    clippy::too_many_lines,
    reason = "one plugin command walks list, install, and reload in place"
)]
pub async fn run(
    command: PluginCommand,
    plugins: &[ConfiguredPlugin],
    grants: &GrantStore,
    by: ClientId,
    stdout: &mut impl Write,
) -> Result<ExitCode, PluginCommandError> {
    let mut plugins: Vec<&ConfiguredPlugin> = plugins.iter().collect();
    plugins.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));

    match command {
        PluginCommand::Grant { name } => {
            let plugin = plugins
                .iter()
                .find(|plugin| plugin.name == name)
                .copied()
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        crate::cli::texts::plugin_not_configured(&name.to_string()),
                    )
                })?;
            if plugin.origin == Origin::Builtin {
                return Err(PluginCommandError::Builtin);
            }
            if plugin.capabilities.is_empty() {
                writeln!(
                    stdout,
                    "{}",
                    crate::cli::texts::plugin_declares_no_services(&plugin.name.to_string())
                )?;
                return Ok(ExitCode::SUCCESS);
            }
            let key = GrantKey {
                extension: plugin.name.clone(),
                origin: plugin.origin,
                services: plugin.capabilities,
            };
            let inserted = grants.grant(key, by, jiff::Timestamp::now()).await?;
            let services = display_services(plugin.capabilities);
            let origin = display_origin(plugin.origin);
            if inserted {
                writeln!(
                    stdout,
                    "{}",
                    crate::cli::texts::plugin_granted(&plugin.name.to_string(), origin, &services)
                )?;
            } else {
                writeln!(
                    stdout,
                    "{}",
                    crate::cli::texts::plugin_already_granted(
                        &plugin.name.to_string(),
                        origin,
                        &services
                    )
                )?;
            }
            Ok(ExitCode::SUCCESS)
        }
        PluginCommand::Revoke { name } => {
            let removed = grants.revoke(&name).await?;
            if removed == 0 {
                writeln!(
                    stdout,
                    "{}",
                    crate::cli::texts::plugin_no_grants(&name.to_string())
                )?;
            } else {
                writeln!(
                    stdout,
                    "{}",
                    crate::cli::texts::plugin_revoked(&name.to_string(), removed)
                )?;
            }
            Ok(ExitCode::SUCCESS)
        }
        PluginCommand::List => {
            if plugins.is_empty() {
                writeln!(stdout, "{}", crate::cli::texts::PLUGIN_NONE)?;
                return Ok(ExitCode::SUCCESS);
            }
            for plugin in plugins {
                let status = plugin.status(grants).await?;
                let services = if plugin.capabilities.is_empty() {
                    "none".to_owned()
                } else {
                    display_services(plugin.capabilities)
                };
                let status = match status {
                    GrantStatus::Granted => "granted",
                    GrantStatus::NotGranted => "not granted",
                    GrantStatus::NotRequired => "not required",
                };
                writeln!(
                    stdout,
                    "{}",
                    crate::cli::texts::plugin_list_row(
                        &plugin.name.to_string(),
                        display_origin(plugin.origin),
                        &services,
                        status
                    )
                )?;
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn display_origin(origin: Origin) -> &'static str {
    match origin {
        Origin::User => "user",
        Origin::Bundled => "bundled",
        _ => "builtin",
    }
}

fn display_services(services: ServiceSet) -> String {
    let mut names = Vec::new();
    for service in services.iter() {
        if service.capability().is_none() {
            continue;
        }
        names.push(service.as_str());
    }
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::{ConfiguredPlugin, PluginCommand};
    use dal_agent::{GrantKey, GrantStore};
    use dal_core::{ClientId, Name, Origin, ServiceSet};

    fn configured(name: &str, origin: Origin, services: &[&str]) -> ConfiguredPlugin {
        ConfiguredPlugin {
            name: name.parse::<Name>().unwrap(),
            origin,
            capabilities: ServiceSet::from_names(services.iter().copied()).unwrap(),
        }
    }

    #[tokio::test]
    async fn grant_persists_exact_declared_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let grants = GrantStore::new(dir.path().to_path_buf());
        let plugins = vec![configured("focus", Origin::User, &["fs.read", "net"])];
        let mut stdout = Vec::new();
        let code = super::run(
            PluginCommand::Grant {
                name: "focus".parse::<Name>().unwrap(),
            },
            &plugins,
            &grants,
            ClientId::new("test"),
            &mut stdout,
        )
        .await
        .unwrap();
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert_eq!(stdout, b"Granted \"focus\" (user): fs.read, net.\n");
        let changed = vec![configured("focus", Origin::User, &["fs.read"])];
        let mut stdout = Vec::new();
        super::run(
            PluginCommand::List,
            &changed,
            &grants,
            ClientId::new("test"),
            &mut stdout,
        )
        .await
        .unwrap();
        let text = String::from_utf8(stdout).unwrap();
        assert!(text.contains("not granted"), "{text}");
    }

    #[tokio::test]
    async fn revoke_is_idempotent_and_reports_session_scope() {
        let dir = tempfile::tempdir().unwrap();
        let grants = GrantStore::new(dir.path().to_path_buf());
        let plugins = vec![configured("focus", Origin::User, &["fs.read", "net"])];
        let mut stdout = Vec::new();
        super::run(
            PluginCommand::Grant {
                name: "focus".parse::<Name>().unwrap(),
            },
            &plugins,
            &grants,
            ClientId::new("test"),
            &mut stdout,
        )
        .await
        .unwrap();
        grants
            .grant(
                GrantKey {
                    extension: "focus".parse::<Name>().unwrap(),
                    origin: Origin::User,
                    services: ServiceSet::from_names(["net"]).unwrap(),
                },
                ClientId::new("test"),
                jiff::Timestamp::now(),
            )
            .await
            .unwrap();
        let mut stdout = Vec::new();
        let code = super::run(
            PluginCommand::Revoke {
                name: "focus".parse::<Name>().unwrap(),
            },
            &plugins,
            &grants,
            ClientId::new("test"),
            &mut stdout,
        )
        .await
        .unwrap();
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert_eq!(
            stdout,
            b"Revoked \"focus\": removed 2 persistent grant(s). Session grants remain until their sessions end.\n"
        );
        let mut stdout = Vec::new();
        super::run(
            PluginCommand::Revoke {
                name: "focus".parse::<Name>().unwrap(),
            },
            &plugins,
            &grants,
            ClientId::new("test"),
            &mut stdout,
        )
        .await
        .unwrap();
        assert_eq!(stdout, b"No persistent grants for \"focus\".\n");
    }

    #[tokio::test]
    async fn list_is_byte_sorted_and_handles_empty_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let grants = GrantStore::new(dir.path().to_path_buf());
        let plugins = vec![
            configured("zeta", Origin::User, &["net"]),
            configured("beta", Origin::Bundled, &["fs.read"]),
            configured("alpha", Origin::User, &[]),
        ];
        let mut stdout = Vec::new();
        super::run(
            PluginCommand::List,
            &plugins,
            &grants,
            ClientId::new("test"),
            &mut stdout,
        )
        .await
        .unwrap();
        assert_eq!(
            stdout,
            b"alpha (user): services: none; persistent grant: not required\nbeta (bundled): services: fs.read; persistent grant: not granted\nzeta (user): services: net; persistent grant: not granted\n"
        );
        let mut stdout = Vec::new();
        super::run(
            PluginCommand::List,
            &[],
            &grants,
            ClientId::new("test"),
            &mut stdout,
        )
        .await
        .unwrap();
        assert_eq!(stdout, b"No plugins configured.\n");
    }
}
