//! MCP client runtime surface.
//!
//! The host holds at most one client per generation; `Services::mcp` routes
//! through it. When no client is registered the service fails closed with
//! `ServiceError::Denied(DenyReason::Unavailable { what: "MCP client" })`.

use super::generation::Generation;
use super::{BoxFuture, Caller, CallerKind};
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

/// Resolves a call to exactly one declaration before any transport can start.
///
/// Direct callers may use only their own declarations. The extension that
/// registered the MCP client may also call declarations belonging to other
/// extensions, as mapped tools do. Ambiguous server names fail closed.
pub(crate) fn declared_call_grant(
    generation: &Generation,
    who: &Caller,
    server: &str,
) -> Result<(Caller, Box<str>, Box<str>), ServiceError> {
    let registrar = generation
        .mcp()
        .and_then(|(index, _)| generation.extensions.get(index))
        .is_some_and(|extension| {
            extension.name() == who.ext().as_str()
                && extension.origin() == who.origin()
                && extension.inject() == who.inject
                && extension.state_version() == who.state_version()
        });
    let qualified = server.rsplit_once('.');
    let mut matches = generation.mcp_declarations().into_iter().filter(|declaration| {
        (registrar || declaration.plugin == *who.ext())
            && declaration.block.servers.keys().any(|name| {
                name.as_ref() == server
                    || qualified.is_some_and(|(skill, server)| {
                        declaration.skill.as_str() == skill && name.as_ref() == server
                    })
            })
    });
    let declaration = matches
        .next()
        .ok_or(ServiceError::Denied(DenyReason::NotGranted))?;
    if matches.next().is_some() {
        return Err(ServiceError::Denied(DenyReason::NotGranted));
    }
    let owner = generation
        .extensions
        .iter()
        .find(|extension| extension.name() == declaration.plugin.as_str())
        .ok_or(ServiceError::Denied(DenyReason::NotGranted))?;
    let grant_caller = if declaration.plugin == *who.ext() {
        who.clone()
    } else {
        Caller::new(
            declaration.plugin.clone(),
            owner.origin(),
            owner.inject(),
            owner.state_version(),
            CallerKind::Tool,
            who.turn(),
        )
    };
    let (set, detail) = mcp_grant_request(generation, &declaration.plugin)?;
    Ok((grant_caller, set, detail))
}

/// Canonical, current server-set digest and redacted approval text.
///
/// Both mapped and scripted callers use these exact bytes as their grant
/// identity. Any changed target, including an stdio command, changes the set.
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

/// One redacted `<skill>.<server>: <target>` line per declared server.
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
