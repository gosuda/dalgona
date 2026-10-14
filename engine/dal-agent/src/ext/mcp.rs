//! MCP client runtime surface.
//!
//! The host holds at most one client per generation; `Services::mcp` routes
//! through it. When no client is registered the service fails closed with
//! `ServiceError::Denied(DenyReason::Unavailable { what: "MCP client" })`.

use std::collections::BTreeSet;

use super::generation::Generation;
use super::{BoxFuture, Caller};
use crate::error::ServiceError;
use dal_core::ext::{McpDeclaration, McpRequest, McpResponse};
use dal_core::{DenyReason, Name, Service};

/// Host-configured bridge from `Services::mcp` to MCP servers.
pub trait McpClient: Send + Sync + 'static {
    /// Calls one MCP tool on behalf of `who`.
    fn call<'a>(
        &'a self,
        who: &'a Caller,
        req: McpRequest,
    ) -> BoxFuture<'a, Result<McpResponse, ServiceError>>;
}

pub(crate) fn mcp_grant_request(
    generation: &Generation,
    plugin: &Name,
) -> Result<(Box<str>, Box<str>), ServiceError> {
    let mut declarations: Vec<McpDeclaration> = generation
        .mcp_declarations()
        .into_iter()
        .filter(|declaration| declaration.plugin == *plugin)
        .collect();
    declarations.sort_by(|left, right| left.skill.cmp(&right.skill));
    if declarations.is_empty() {
        return Err(ServiceError::failed(
            Some(Service::Mcp),
            format!("extension \"{plugin}\" has no declared MCP servers"),
        ));
    }
    let canonical = sonic_rs::to_string(&declarations)
        .map_err(|error| ServiceError::failed(Some(Service::Mcp), error.to_string()))?;
    let digest = blake3::hash(canonical.as_bytes()).to_hex().to_string();
    let mut detail = Vec::new();
    for declaration in &declarations {
        detail.extend(grant_target_lines(declaration));
    }
    detail.sort();
    Ok((digest.into(), detail.join("\n").into()))
}

/// One redacted `<skill>.<server>: <target>` line per declared server. The
/// target text comes from the declaration's own single renderer, so the grant
/// prompt can never disagree with the approval surface about what is public.
fn grant_target_lines(declaration: &McpDeclaration) -> Vec<String> {
    declaration
        .block
        .servers
        .iter()
        .map(|(server, server_decl)| {
            format!(
                "{}.{}: {}",
                declaration.skill,
                server,
                server_decl.redacted_target()
            )
        })
        .collect()
}

/// Finds every declaring plugin that the MCP client could select for `server`.
///
/// A bare server name can match multiple skills. Authorization must cover
/// every matching owner because the MCP client can select any of them.
pub(crate) fn mcp_grant_plugins(
    generation: &Generation,
    server: &str,
) -> Result<Vec<Name>, ServiceError> {
    let qualified = server.rsplit_once('.');
    let mut plugins = BTreeSet::new();
    for declaration in generation.mcp_declarations() {
        let matches = declaration.block.servers.contains_key(server)
            || qualified.is_some_and(|(skill, server)| {
                declaration.skill.as_str() == skill
                    && declaration.block.servers.contains_key(server)
            });
        if matches {
            plugins.insert(declaration.plugin);
        }
    }
    if plugins.is_empty() {
        return Err(ServiceError::Denied(DenyReason::NotGranted));
    }
    Ok(plugins.into_iter().collect())
}
