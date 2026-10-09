//! The session tool overlay: tools an extension registers for one session at
//! run time, published to turns only at turn boundaries.
//!
//! An extension stages its tools through [`Overlay::stage`]; a turn sees them
//! only through the [`TurnTools`] snapshot it takes when it starts, so the
//! advertised tool bytes never change inside a turn. A `Deferred` tool
//! resolves and runs like any other; the fold journals `ToolPromoted` when
//! its first call succeeds, and the next turn's snapshot lists it as `Model`.
//! Entries drop when their owner leaves the generation, when the session
//! closes, and when the owner replaces its set.

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, PoisonError};

use dal_core::ext::{McpBlock, McpDeclaration, McpServerDecl};
use dal_core::{Name, Origin, Service, ServiceSet, Visibility};
use tokio_util::sync::CancellationToken;

use super::generation::Generation;
use super::grants::GrantStore;
use super::tool::Tool;
use super::{Caller, CallerKind};
use crate::error::ServiceError;
use crate::ext::Extension;

/// One staged or published overlay tool.
#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) owner: Name,
    pub(crate) mcp: Option<DeclaredMcp>,
    pub(crate) tool: Arc<dyn Tool>,
    pub(crate) visibility: Visibility,
}

/// The extension and skill whose block declares an overlay tool.
#[derive(Clone)]
pub(crate) struct DeclaredMcp {
    pub(crate) plugin: Name,
    pub(crate) skill: Name,
    pub(crate) origin: Origin,
    pub(crate) inject: ServiceSet,
    pub(crate) block: McpBlock,
}

/// The session's overlay table; the services stage into it and turns snapshot it.
#[derive(Default)]
pub(crate) struct Overlay {
    staged: Mutex<BTreeMap<Name, Vec<Entry>>>,
    grant_runtime: Option<GrantRuntime>,
}

#[derive(Clone)]
struct GrantRuntime {
    grants: Arc<GrantStore>,
    cancel: CancellationToken,
}

impl Overlay {
    /// Builds the session overlay with its shared capability gate.
    pub(crate) fn with_grants(grants: Arc<GrantStore>, cancel: CancellationToken) -> Self {
        Self {
            staged: Mutex::new(BTreeMap::new()),
            grant_runtime: Some(GrantRuntime { grants, cancel }),
        }
    }

    /// Replaces `owner`'s staged tools with `tools`; an empty list clears them.
    ///
    /// Nothing is staged unless every tool passes: a name may not repeat in
    /// the batch, name a tool of the generation, or name another owner's
    /// staged tool.
    pub(crate) fn stage(
        &self,
        generation: &Generation,
        owner: &Name,
        tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> Result<(), ServiceError> {
        let mut staged = self.staged.lock().unwrap_or_else(PoisonError::into_inner);
        let mut batch = BTreeSet::new();
        for (tool, _) in &tools {
            let name = tool.name();
            let held_by = if !batch.insert(name.clone()) {
                Some(owner.as_str().into())
            } else if let Some(entry) = generation.tools.find(name) {
                Some(generation_owner(generation, entry.ext))
            } else {
                staged
                    .iter()
                    .filter(|(other, _)| *other != owner)
                    .find(|(_, entries)| entries.iter().any(|e| e.tool.name() == name))
                    .map(|(other, _)| other.as_str().into())
            };
            if let Some(held_by) = held_by {
                return Err(ServiceError::ToolNameInUse {
                    name: name.as_str().into(),
                    held_by,
                });
            }
        }
        if tools.is_empty() {
            staged.remove(owner);
            return Ok(());
        }
        let mut entries = Vec::with_capacity(tools.len());
        for (tool, visibility) in tools {
            let mcp = declaration_for_tool(generation, tool.as_ref())?;
            entries.push(Entry {
                owner: owner.clone(),
                mcp,
                tool,
                visibility,
            });
        }
        staged.insert(owner.clone(), entries);
        Ok(())
    }

    /// Takes the turn-boundary snapshot against `generation`.
    ///
    /// Owners the generation no longer holds are dropped for good, and a tool
    /// whose name the generation now claims is left out.
    pub(crate) fn publish(
        &self,
        generation: &Generation,
        promoted: Arc<BTreeSet<Name>>,
    ) -> TurnTools {
        let mut staged = self.staged.lock().unwrap_or_else(PoisonError::into_inner);
        staged.retain(|owner, entries| {
            let owner_is_live = generation
                .extensions
                .iter()
                .any(|extension| extension.name() == owner.as_str());
            if !owner_is_live {
                return false;
            }
            entries.retain(|entry| declaration_is_live(generation, entry));
            !entries.is_empty()
        });
        let mut entries: Vec<Entry> = staged
            .values()
            .flatten()
            .filter(|entry| generation.tools.find(entry.tool.name()).is_none())
            .cloned()
            .collect();
        entries.sort_by(|left, right| left.tool.name().cmp(right.tool.name()));
        TurnTools {
            entries: entries.into(),
            promoted,
            grant_runtime: self.grant_runtime.clone(),
            allowed: None,
        }
    }

    /// Drops every staged tool; the session is ending.
    pub(crate) fn clear(&self) {
        self.staged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

fn declaration_for_tool(
    generation: &Generation,
    tool: &dyn Tool,
) -> Result<Option<DeclaredMcp>, ServiceError> {
    let Some(plugin) = tool.declaring_extension() else {
        return Ok(None);
    };
    let name = tool.name();
    let (skill_text, tail) = name
        .as_str()
        .split_once('.')
        .ok_or_else(|| invalid_mcp_tool(name, &plugin, name.as_str(), "missing server segment"))?;
    let (server, _remote) = tail.split_once('.').ok_or_else(|| {
        invalid_mcp_tool(name, &plugin, skill_text, "missing remote tool segment")
    })?;
    if server.is_empty() {
        return Err(invalid_mcp_tool(
            name,
            &plugin,
            skill_text,
            "server name must be non-empty",
        ));
    }
    let skill = Name::parse(skill_text)
        .map_err(|_| invalid_mcp_tool(name, &plugin, skill_text, "skill name is invalid"))?;
    let extension = generation
        .extensions
        .iter()
        .find(|extension| extension.name() == plugin.as_str())
        .ok_or_else(|| {
            invalid_mcp_tool(
                name,
                &plugin,
                skill_text,
                "declaring extension is not loaded",
            )
        })?;
    if !extension.inject().contains(Service::Mcp) {
        return Err(invalid_mcp_tool(
            name,
            &plugin,
            skill_text,
            "declaring extension does not inject mcp",
        ));
    }
    let record = extension
        .skills()
        .iter()
        .find(|record| record.name == skill)
        .ok_or_else(|| {
            invalid_mcp_tool(name, &plugin, skill_text, "declaring skill is not loaded")
        })?;
    let block = record
        .mcp
        .as_ref()
        .ok_or_else(|| invalid_mcp_tool(name, &plugin, skill_text, "skill has no MCP servers"))?;
    if !block.servers.contains_key(server) {
        return Err(invalid_mcp_tool(
            name,
            &plugin,
            skill_text,
            "declared server is missing",
        ));
    }
    Ok(Some(DeclaredMcp {
        plugin,
        skill,
        origin: extension.origin(),
        inject: extension.inject(),
        block: block.clone(),
    }))
}

fn invalid_mcp_tool(tool: &Name, plugin: &Name, skill: &str, reason: &'static str) -> ServiceError {
    ServiceError::McpToolDeclaration {
        tool: tool.as_str().into(),
        plugin: plugin.as_str().into(),
        skill: skill.into(),
        reason,
    }
}

fn declaration_is_live(generation: &Generation, entry: &Entry) -> bool {
    let Some(declaration) = &entry.mcp else {
        return true;
    };
    generation.extensions.iter().any(|extension| {
        extension.name() == declaration.plugin.as_str()
            && extension.skills().iter().any(|skill| {
                skill.name == declaration.skill && skill.mcp.as_ref() == Some(&declaration.block)
            })
    })
}

fn mcp_grant_request(
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
    for declaration in declarations {
        for (server, server_decl) in declaration.block.servers {
            let transport = match server_decl {
                McpServerDecl::Stdio { command, env } => {
                    let env_keys = if env.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " env keys [{}]",
                            env.keys()
                                .map(AsRef::as_ref)
                                .collect::<Vec<&str>>()
                                .join(", ")
                        )
                    };
                    format!("command {:?}{env_keys}", redact_mcp_argv(command))
                }
                McpServerDecl::Http { url } => format!("URL {}", redact_mcp_url(&url)),
            };
            detail.push(format!("{}.{}: {transport}", declaration.skill, server));
        }
    }
    detail.sort();
    Ok((digest.into(), detail.join("\n").into()))
}
fn redact_mcp_argv(command: Vec<Box<str>>) -> Vec<Box<str>> {
    let mut redacted = Vec::with_capacity(command.len());
    let mut redact_next = false;
    for argument in command {
        if is_secret_mcp_flag(&argument) {
            redacted.push(argument);
            redact_next = true;
            continue;
        }
        if let Some((flag, _)) = argument.split_once('=')
            && is_secret_mcp_flag(flag)
        {
            redacted.push(format!("{flag}=<redacted>").into());
            redact_next = false;
            continue;
        }
        if redact_next {
            redacted.push("<redacted>".into());
            redact_next = false;
        } else {
            redacted.push(argument);
        }
    }
    redacted
}

fn is_secret_mcp_flag(argument: &str) -> bool {
    matches!(
        argument,
        "-t" | "--token" | "--api-key" | "--authorization" | "-H" | "--header"
    )
}

fn redact_mcp_url(url: &str) -> String {
    let Some((scheme, remainder)) = url.split_once("://") else {
        return "invalid MCP URL".to_owned();
    };
    let without_query = remainder.split(['?', '#']).next().unwrap_or_default();
    let (authority, path) = without_query
        .split_once('/')
        .map_or((without_query, None), |(authority, path)| {
            (authority, Some(path))
        });
    let authority = authority.rsplit('@').next().unwrap_or_default();
    match path {
        Some(path) => format!("{scheme}://{authority}/{path}"),
        None => format!("{scheme}://{authority}"),
    }
}

fn generation_owner(generation: &Generation, ext: usize) -> Box<str> {
    generation
        .extensions
        .get(ext)
        .map_or("the generation", |extension| extension.name())
        .into()
}

/// The tools one turn sees beyond its generation: the overlay entries and the
/// promoted names, both frozen when the turn started.
#[derive(Clone)]
pub(crate) struct TurnTools {
    entries: Arc<[Entry]>,
    promoted: Arc<BTreeSet<Name>>,
    grant_runtime: Option<GrantRuntime>,
    allowed: Option<Arc<BTreeSet<Name>>>,
}

impl TurnTools {
    /// A snapshot with no overlay entries and no promotions.
    pub(crate) fn empty() -> Self {
        Self {
            entries: Arc::from([]),
            promoted: Arc::new(BTreeSet::new()),
            grant_runtime: None,
            allowed: None,
        }
    }

    /// The visibility the model list applies: a promoted `Deferred` tool is `Model`.
    pub(crate) fn effective(&self, name: &Name, declared: Visibility) -> Visibility {
        if declared == Visibility::Deferred && self.promoted.contains(name) {
            Visibility::Model
        } else {
            declared
        }
    }

    /// Narrows the snapshot to `allowed`; `None` leaves every tool visible.
    ///
    /// Each turn takes a fresh snapshot against the generation current at its
    /// start, so applying the list here keeps it in force across reloads.
    pub(crate) fn restricted_to(mut self, allowed: Option<Arc<BTreeSet<Name>>>) -> Self {
        if let Some(names) = &allowed {
            self.entries = self
                .entries
                .iter()
                .filter(|entry| names.contains(entry.tool.name()))
                .cloned()
                .collect();
        }
        self.allowed = allowed;
        self
    }

    /// Whether the session may see and call `name`.
    pub(crate) fn permits(&self, name: &Name) -> bool {
        self.allowed
            .as_ref()
            .is_none_or(|names| names.contains(name))
    }

    /// Resolves `name` in the generation first, then in the overlay; a name
    /// outside the session's allowlist resolves to nothing.
    pub(crate) fn tool<'a>(
        &'a self,
        generation: &'a Generation,
        name: &Name,
    ) -> Option<(&'a Arc<dyn Tool>, Visibility)> {
        if !self.permits(name) {
            return None;
        }
        generation.tool(name).or_else(|| {
            self.entry(name)
                .map(|entry| (&entry.tool, entry.visibility))
        })
    }

    /// The declaring extension of a mapped tool, otherwise its registrar.
    pub(crate) fn owner(&self, name: &Name) -> Option<&Name> {
        self.entry(name)
            .map(|entry| entry.mcp.as_ref().map_or(&entry.owner, |decl| &decl.plugin))
    }

    /// The MCP skill declaration associated with a mapped tool.
    pub(crate) fn declared_mcp(&self, name: &Name) -> Option<&DeclaredMcp> {
        self.entry(name)?.mcp.as_ref()
    }

    /// Ensures the caller granted the declaring extension's current MCP set.
    pub(crate) async fn authorize_mcp(
        &self,
        generation: &Generation,
        name: &Name,
        turn: dal_core::TurnId,
    ) -> Result<(), ServiceError> {
        let Some(declaration) = self.declared_mcp(name) else {
            return Ok(());
        };
        let Some(runtime) = &self.grant_runtime else {
            return Err(ServiceError::failed(
                None,
                "MCP grant runtime is unavailable",
            ));
        };
        let (set, detail) = mcp_grant_request(generation, &declaration.plugin)?;
        // The version binds to the generation this authorization ran in,
        // not a later reload.
        let state_version = generation
            .extensions
            .iter()
            .find(|ext| ext.name() == declaration.plugin.as_str())
            .map_or(NonZeroU32::MIN, Extension::state_version);
        let caller = Caller::new(
            declaration.plugin.clone(),
            declaration.origin,
            declaration.inject,
            state_version,
            CallerKind::Tool,
            Some(turn),
        );
        runtime
            .grants
            .ensure_declared_mcp(&caller, set, detail, &runtime.cancel)
            .await
            .map(|_| ())
            .map_err(|error| error.naming_grant(Service::Mcp, declaration.plugin.as_str()))
    }

    /// The published overlay entries in name order.
    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn entry(&self, name: &Name) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.tool.name() == name)
    }
}
