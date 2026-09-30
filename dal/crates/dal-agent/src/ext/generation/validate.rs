//! Generation validation: the [`ValidatedExtensions`] proof.
//!
//! Validation sorts the batch into canonical order, then claims every
//! `(record kind, name)` pair against the previous generation and within
//! the batch. Only [`super::Generation::build`] consumes the proof, so a
//! failed reload publishes nothing.

use std::cmp::Ordering;

use dal_core::{Claimant, Name, Origin, RegistrationError, Service};

use crate::ext::Extension;

use super::Generation;
use super::tables::ClaimTables;

/// Sort rank of one extension origin: builtins keep input order, then
/// bundled by name, then user by name. Unknown future origins sort last.
fn origin_rank(origin: Origin) -> u8 {
    match origin {
        Origin::Builtin => 0,
        Origin::Bundled => 1,
        _ => 2,
    }
}

/// Maps one extension to the claimant its records attribute conflicts to
/// plus its parsed name: builtins blame the built-in extension, bundled and
/// user extensions blame the registering plugin. The name rides along so
/// non-`Name` record keys (model ids, `skill:` commands) can still report a
/// [`RegistrationError::Conflict`], which carries a [`Name`].
pub(crate) fn claimant_of(ext: &Extension) -> Result<(Claimant, Name), RegistrationError> {
    let name = Name::parse(ext.name())?;
    let claimant = match ext.origin() {
        Origin::Builtin => Claimant::Builtin(name.clone()),
        _ => Claimant::Plugin(name.clone()),
    };
    Ok((claimant, name))
}

/// Maps one record key to the [`Name`] a conflict reports: the key text
/// when it matches the name grammar, else the owning extension's name.
pub(crate) fn conflict_name(key: &str, owner: &Name) -> Name {
    match Name::parse(key) {
        Ok(name) => name,
        Err(_) => owner.clone(),
    }
}

fn validate_mcp_skill_inject(extension: &Extension) -> Result<(), RegistrationError> {
    if extension.inject().contains(Service::Mcp) {
        return Ok(());
    }
    let owner = Name::parse(extension.name())?;
    if let Some(skill) = extension.skills().iter().find(|skill| skill.mcp.is_some()) {
        return Err(RegistrationError::McpSkillNotInjected {
            skill: skill.name.clone(),
            extension: owner,
        });
    }
    Ok(())
}

fn validate_mcp_tools(extensions: &[Extension]) -> Result<(), RegistrationError> {
    for registering in extensions {
        for (tool, _) in registering.tools() {
            let Some(extension_name) = tool.declaring_extension() else {
                continue;
            };
            let invalid = |reason| RegistrationError::InvalidMcpTool {
                tool: tool.name().as_str().into(),
                extension: extension_name.clone(),
                reason,
            };
            let (skill_text, tail) = tool
                .name()
                .as_str()
                .split_once('.')
                .ok_or_else(|| invalid("mapped tool name must be <skill>.<server>.<tool>"))?;
            let (server, remote) = tail
                .split_once('.')
                .ok_or_else(|| invalid("mapped tool name must be <skill>.<server>.<tool>"))?;
            if server.is_empty() || remote.is_empty() {
                return Err(invalid("server and remote tool names must be non-empty"));
            }
            let skill_name = Name::parse(skill_text)
                .map_err(|_| invalid("mapped tool skill name is invalid"))?;
            let declaring = extensions
                .iter()
                .find(|extension| extension.name() == extension_name.as_str())
                .ok_or_else(|| invalid("declaring extension is not loaded"))?;
            let skill = declaring
                .skills()
                .iter()
                .find(|skill| skill.name == skill_name)
                .ok_or_else(|| invalid("declaring skill is not loaded"))?;
            let block = skill
                .mcp
                .as_ref()
                .ok_or_else(|| invalid("declaring skill has no MCP servers"))?;
            if !block.servers.contains_key(server) {
                return Err(invalid("declared server is missing"));
            }
        }
    }
    Ok(())
}

/// Type-state proof that every extension below passed registration and
/// cross-extension conflict checks. Only [`super::Generation::build`]
/// consumes it; the host publishes nothing until validation succeeds.
#[derive(Debug)]
pub(crate) struct ValidatedExtensions(pub(crate) Vec<Extension>);

impl ValidatedExtensions {
    /// Validates `extensions` as one publishable set.
    ///
    /// The batch sorts into canonical order first, then every duplicate
    /// `(record kind, name)` pair within the batch and against `prev` fails
    /// with [`RegistrationError::Conflict`]. A second MCP client fails the
    /// same way with kind `"mcp_client"` and the second extension's name.
    ///
    /// Validate in stages: the product set with `prev` as `None`, then each
    /// further set with `prev` holding the accumulated generation, so any
    /// shadowing across stages errors. A full-set reload revalidates
    /// standalone with `prev` as `None`.
    pub(crate) fn validate(
        extensions: Vec<Extension>,
        prev: Option<&Generation>,
    ) -> Result<Self, RegistrationError> {
        let mut sorted = extensions;
        sorted.sort_by(|a, b| {
            origin_rank(a.origin())
                .cmp(&origin_rank(b.origin()))
                .then_with(|| {
                    if matches!(a.origin(), Origin::Builtin) {
                        Ordering::Equal
                    } else {
                        a.name().cmp(b.name())
                    }
                })
        });
        let mut tables = ClaimTables::new();
        if let Some(prev) = prev {
            for ext in prev.extensions.iter() {
                tables.seed(ext)?;
            }
        }
        for ext in &sorted {
            validate_mcp_skill_inject(ext)?;
        }
        validate_mcp_tools(&sorted)?;
        for ext in &sorted {
            tables.check(ext)?;
        }
        Ok(Self(sorted))
    }
}
