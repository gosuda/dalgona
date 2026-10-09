//! Immutable extension generation: validation proof plus indexed snapshot.
//!
//! A [`Generation`] is the single immutable snapshot every turn and handler
//! holds through `Arc`: turns retain their snapshot, in-flight calls keep old
//! handlers alive, and a failed reload publishes nothing because the host
//! keeps its old `Arc` while [`ValidatedExtensions::validate`] fails before
//! [`Generation::build`] runs. `build` never mutates published state; it
//! consumes a validation proof and mints a fresh [`GenerationId`].
//!
//! Canonical extension order: `Builtin` extensions retain input order,
//! `Bundled` extensions sort by name, `User` extensions sort by name (both
//! sorts stable). Hook, watcher, and record tables follow that order, and
//! registration order within one extension is preserved.
//!
//! A deferred tool remains in this immutable generation. The session fold
//! journals its first successful call as `ToolPromoted`; the next turn's
//! `TurnTools` snapshot applies that set when projecting visibility. Request
//! assembly uses that snapshot to gate `tool_search`.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use dal_core::ext::{BeforeRequest, BeforeTurn, ExportId, McpDeclaration, TurnEnd};
use dal_core::{
    CommandName, CommandSpec, GenerationId, InputEvent, InputVerdict, ModelId, ModelInfo,
    ModelRoute, Name, Origin, RequestParams, SessionEnd, SessionStart, Settled, ToolCallEvent,
    ToolCallVerdict, ToolResultEvent, ToolSpec, Visibility,
};

use super::prompt::PromptSection;
use super::tool::Tool;
use super::{
    CommandHandler, Compactor, Extension, Hook, McpClient, ObserveHook, SchemeResolver,
    StatusRecord, WatchFactory,
};

pub mod catalog;
mod tables;
mod validate;

use crate::ext::docs::{DocPage, DocTable};
use crate::ext::script::Evidence;
use catalog::Catalog;

pub(crate) use tables::{
    CommandEntry, CommandTable, CompactorTable, NamedEntry, SchemeTable, ToolEntry, ToolTable,
};
pub(crate) use validate::ValidatedExtensions;

/// Monotonic counter backing [`Generation::build`]'s fresh identifier.
///
/// Starts at one so the first generation is nonzero by construction; the
/// zero fallback below only runs after more builds than fit in a `u64`.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Mints a fresh nonzero generation identifier.
fn next_id() -> GenerationId {
    let raw = NEXT_GENERATION.fetch_add(1, AtomicOrdering::Relaxed);
    match NonZeroU64::new(raw) {
        Some(id) => GenerationId::new(id),
        None => GenerationId::new(NonZeroU64::MIN),
    }
}

/// Splices fresh plugin extensions onto the kept product prefix: the
/// reload publication point's set operation.
///
/// The contract the Starlark loader upholds: every plugin extension
/// carries [`Origin::User`] and nothing else mints it, so the kept
/// prefix is every non-`User` extension in current canonical order
/// (builtins in input order, then bundled by name) and only the
/// trailing `User` run is replaced. `replace` swaps same-named
/// non-`User` records, so a reload can rebind per-generation state
/// inside a builtin extension. Canonical validation re-sorts
/// afterward, so the result order never depends on this filter.
pub(crate) fn splice_plugins(
    current: &[Extension],
    plugins: Vec<Extension>,
    replace: Vec<Extension>,
) -> Vec<Extension> {
    let replaced: std::collections::BTreeSet<&str> = replace.iter().map(Extension::name).collect();
    let mut next: Vec<Extension> = current
        .iter()
        .filter(|ext| ext.origin() != Origin::User && !replaced.contains(ext.name()))
        .cloned()
        .collect();
    next.extend(replace);
    next.extend(plugins);
    next
}

#[cfg(test)]
mod tests;

/// One immutable, fully indexed extension snapshot.
///
/// Turns hold this through `Arc`; replacing it publishes a new `Arc` while
/// old holders keep theirs, so old tools and handlers stay live and a
/// failed validation publishes nothing.
pub(crate) struct Generation {
    pub(crate) id: GenerationId,
    pub(crate) extensions: Arc<[Extension]>,
    pub(crate) tools: ToolTable,
    pub(crate) commands: CommandTable,
    pub(crate) prompt_sections: Arc<[PromptSection]>,
    pub(crate) schemes: SchemeTable,
    pub(crate) compactors: CompactorTable,
    /// The generation's single MCP client with its owning extension index.
    /// Deviation from the plan literal (`Option<Arc<..>>`): the index gives
    /// `Caller` attribution without a second scan; `mcp()` exposes both.
    pub(crate) mcp_client: Option<(usize, Arc<dyn McpClient>)>,
    /// Every doc page, published atomically with the rest of the snapshot.
    /// Shared by handle so resolvers read the same table the host serves.
    pub(crate) docs: Arc<DocTable>,
    /// Every registered status kind with its poll, in canonical extension
    /// order; the host shutdown ladder polls these to quiet.
    pub(crate) status_kinds: Arc<[StatusRecord]>,
    /// The operation catalog scripts describe, check, and bind through (R03).
    pub(crate) catalog: Catalog,
    /// The generation's single evidence owner, when one is installed (R06 E06).
    pub(crate) evidence: Option<Arc<dyn Evidence>>,
    spec_cache: Mutex<HashMap<(Name, ModelId, ModelRoute), Arc<ToolSpec>>>,
}

impl Generation {
    /// Indexes validated extensions infallibly: validation already rejected
    /// every conflict, so this only orders and points. The extensions must
    /// already sit in canonical order, which [`ValidatedExtensions`] owns.
    pub(crate) fn build(validated: ValidatedExtensions) -> Self {
        let extensions: Arc<[Extension]> = validated.0.into();
        let mut tools = Vec::new();
        let mut commands = Vec::new();
        let mut prompt_sections = Vec::new();
        let mut schemes = Vec::new();
        let mut compactors = Vec::new();
        let mut status_kinds = Vec::new();
        let mut mcp_client: Option<(usize, Arc<dyn McpClient>)> = None;
        let mut pages = Vec::new();
        for (index, ext) in extensions.iter().enumerate() {
            for (record, (tool, visibility)) in ext.tools().iter().enumerate() {
                tools.push(ToolEntry {
                    name: tool.name().clone(),
                    ext: index,
                    record,
                    visibility: *visibility,
                });
            }
            for (record, (spec, _)) in ext.commands().iter().enumerate() {
                commands.push(CommandEntry {
                    name: spec.name.clone(),
                    ext: index,
                    record,
                });
            }
            if let Some((kind, poll)) = ext.status() {
                status_kinds.push(StatusRecord {
                    kind: kind.into(),
                    poll: Arc::clone(poll),
                });
            }
            if let Some(section) = ext.prompt_section() {
                prompt_sections.push(section.clone());
            }
            for (record, (name, _)) in ext.schemes().iter().enumerate() {
                schemes.push(NamedEntry {
                    name: name.clone(),
                    ext: index,
                    record,
                });
            }
            for (record, (name, _)) in ext.compactors().iter().enumerate() {
                compactors.push(NamedEntry {
                    name: name.clone(),
                    ext: index,
                    record,
                });
            }
            if mcp_client.is_none()
                && let Some(client) = ext.mcp_clients().first()
            {
                mcp_client = Some((index, Arc::clone(client)));
            }
            for record in ext.docs() {
                pages.push(DocPage::of(ext.name(), record));
            }
        }
        let catalog = Catalog::build(&extensions, mcp_client.is_some());
        let evidence = extensions
            .iter()
            .find_map(|ext| ext.evidence().first().cloned());
        Self {
            id: next_id(),
            catalog,
            evidence,
            extensions,
            tools: ToolTable {
                entries: tools.into(),
            },
            commands: CommandTable {
                entries: commands.into(),
            },
            prompt_sections: prompt_sections.into(),
            schemes: SchemeTable {
                entries: schemes.into(),
            },
            compactors: CompactorTable {
                entries: compactors.into(),
            },
            mcp_client,
            status_kinds: status_kinds.into(),
            docs: Arc::new(DocTable::publish(pages)),
            spec_cache: Mutex::new(HashMap::new()),
        }
    }
}

impl Generation {
    /// Resolves one tool to its record and visibility, unfiltered: dispatch
    /// needs Deferred tools for promotion-on-call and `EvalOnly` tools for
    /// exact model errors. The request assembler filters the model list
    /// through `tool_visibility` and `has_deferred`.
    pub(crate) fn tool(&self, name: &Name) -> Option<(&Arc<dyn Tool>, Visibility)> {
        let entry = self.tools.find(name)?;
        let (tool, _) = self.extensions.get(entry.ext)?.tools().get(entry.record)?;
        Some((tool, entry.visibility))
    }

    /// Reports one tool's visibility, when registered.
    pub(crate) fn tool_visibility(&self, name: &Name) -> Option<Visibility> {
        self.tools.find(name).map(|entry| entry.visibility)
    }

    /// Returns the model-visible spec for one tool, cached per
    /// `(tool name, model id, model route)`. The route joins the key so two
    /// routes under one id can never share a cached schema. A poisoned
    /// cache still returns correct specs; it only skips storing them.
    pub(crate) fn tool_spec(
        &self,
        name: &Name,
        model: &ModelInfo,
        model_id: &ModelId,
    ) -> Option<Arc<ToolSpec>> {
        let key = (name.clone(), model_id.clone(), model.route.clone());
        if let Ok(cache) = self.spec_cache.lock()
            && let Some(hit) = cache.get(&key)
        {
            return Some(Arc::clone(hit));
        }
        let (tool, _) = self.tool(name)?;
        let spec = tool.spec(model);
        if let Ok(mut cache) = self.spec_cache.lock() {
            cache.insert(key, Arc::clone(&spec));
        }
        Some(spec)
    }

    /// Resolves one command to its spec and handler.
    pub(crate) fn command(
        &self,
        name: &CommandName,
    ) -> Option<(&CommandSpec, &Arc<dyn CommandHandler>)> {
        let entry = self.commands.find(name)?;
        let (spec, handler) = self
            .extensions
            .get(entry.ext)?
            .commands()
            .get(entry.record)?;
        Some((spec, handler))
    }

    /// Resolves one scheme to its resolver.
    pub(crate) fn scheme(&self, name: &str) -> Option<&Arc<dyn SchemeResolver>> {
        let entry = self.schemes.find(name)?;
        let (_, resolver) = self
            .extensions
            .get(entry.ext)?
            .schemes()
            .get(entry.record)?;
        Some(resolver)
    }

    /// Resolves one compactor.
    pub(crate) fn compactor(&self, name: &str) -> Option<&Arc<dyn Compactor>> {
        let entry = self.compactors.find(name)?;
        let (_, compactor) = self
            .extensions
            .get(entry.ext)?
            .compactors()
            .get(entry.record)?;
        Some(compactor)
    }

    pub(crate) fn mcp(&self) -> Option<(usize, &Arc<dyn McpClient>)> {
        let (ext, client) = self.mcp_client.as_ref()?;
        Some((*ext, client))
    }

    /// Resolves a synthetic model id to its owning extension index and record.
    pub(crate) fn model(&self, id: &str) -> Option<(usize, &super::ModelRecord)> {
        let location = self.catalog.model_route(id)?;
        let record = self
            .extensions
            .get(location.extension)?
            .models()
            .get(location.record)?;
        Some((location.extension, record))
    }

    pub(crate) fn model_export(&self, id: &ExportId) -> Option<(usize, &super::ModelRecord)> {
        let export = self.catalog.model_export(id)?;
        let record = self
            .extensions
            .get(export.extension)?
            .models()
            .get(export.record)?;
        if record.id != export.route || record.export.as_ref() != Some(id) {
            return None;
        }
        Some((export.extension, record))
    }

    /// Lists every MCP block a skill declares, in generation extension order
    /// then registration order within an extension.
    pub(crate) fn mcp_declarations(&self) -> Vec<McpDeclaration> {
        let mut out = Vec::new();
        for extension in self.extensions.iter() {
            let Ok(plugin) = extension.name().parse::<Name>() else {
                continue;
            };
            for skill in extension.skills() {
                if let Some(block) = &skill.mcp {
                    out.push(McpDeclaration {
                        plugin: plugin.clone(),
                        skill: skill.name.clone(),
                        block: block.clone(),
                    });
                }
            }
        }
        out
    }

    /// Borrows every registered status kind in canonical extension order.
    pub(crate) fn status_kinds(&self) -> &[StatusRecord] {
        &self.status_kinds
    }

    /// Borrows every published doc page in URI order.
    pub(crate) fn docs(&self) -> &DocTable {
        &self.docs
    }

    /// Borrows the generation's operation catalog (R03).
    pub(crate) fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Borrows the generation's evidence owner, when one is installed (R06 E06).
    pub(crate) fn evidence(&self) -> Option<&Arc<dyn Evidence>> {
        self.evidence.as_ref()
    }

    /// Borrows one extension's `session_start` hooks in registration order.
    pub(crate) fn session_starts(&self, ext: usize) -> &[Arc<dyn ObserveHook<SessionStart>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.session_starts.as_slice(),
            None => &[],
        }
    }

    /// Borrows one extension's lossless `session_start` hooks.
    pub(crate) fn session_starts_lossless(
        &self,
        ext: usize,
    ) -> &[Arc<dyn ObserveHook<SessionStart>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.session_starts_lossless.as_slice(),
            None => &[],
        }
    }

    /// Borrows one extension's `session_end` hooks in registration order.
    pub(crate) fn session_ends(&self, ext: usize) -> &[Arc<dyn ObserveHook<SessionEnd>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.session_ends.as_slice(),
            None => &[],
        }
    }

    /// Borrows one extension's lossless `session_end` hooks.
    pub(crate) fn session_ends_lossless(&self, ext: usize) -> &[Arc<dyn ObserveHook<SessionEnd>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.session_ends_lossless.as_slice(),
            None => &[],
        }
    }

    /// Borrows one extension's `input` hooks in registration order.
    pub(crate) fn inputs(&self, ext: usize) -> &[Arc<dyn Hook<InputEvent, InputVerdict>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.inputs.as_slice(),
            None => &[],
        }
    }

    /// Borrows one extension's `before_turn` hooks in registration order.
    pub(crate) fn before_turns(&self, ext: usize) -> &[Arc<dyn Hook<BeforeTurn, Option<String>>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.before_turns.as_slice(),
            None => &[],
        }
    }

    /// Borrows one extension's `before_request` hooks in registration order.
    pub(crate) fn before_requests(
        &self,
        ext: usize,
    ) -> &[Arc<dyn Hook<BeforeRequest, Option<RequestParams>>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.before_requests.as_slice(),
            None => &[],
        }
    }

    /// Borrows one extension's `tool_call` hooks in registration order.
    pub(crate) fn tool_calls(
        &self,
        ext: usize,
    ) -> &[Arc<dyn Hook<ToolCallEvent, ToolCallVerdict>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.tool_calls.as_slice(),
            None => &[],
        }
    }

    /// Borrows one extension's `tool_result` hooks in registration order.
    pub(crate) fn tool_results(&self, ext: usize) -> &[Arc<dyn ObserveHook<ToolResultEvent>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.tool_results.as_slice(),
            None => &[],
        }
    }

    /// Borrows one extension's lossless `tool_result` hooks.
    pub(crate) fn tool_results_lossless(
        &self,
        ext: usize,
    ) -> &[Arc<dyn ObserveHook<ToolResultEvent>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.tool_results_lossless.as_slice(),
            None => &[],
        }
    }

    /// Borrows one extension's `turn_end` hooks in registration order.
    pub(crate) fn turn_ends(&self, ext: usize) -> &[Arc<dyn ObserveHook<TurnEnd>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.turn_ends.as_slice(),
            None => &[],
        }
    }

    /// Borrows one extension's lossless `turn_end` hooks.
    pub(crate) fn turn_ends_lossless(&self, ext: usize) -> &[Arc<dyn ObserveHook<TurnEnd>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.turn_ends_lossless.as_slice(),
            None => &[],
        }
    }

    pub(crate) fn settleds(&self, ext: usize) -> &[Arc<dyn ObserveHook<Settled>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.settleds.as_slice(),
            None => &[],
        }
    }

    pub(crate) fn settleds_lossless(&self, ext: usize) -> &[Arc<dyn ObserveHook<Settled>>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.settleds_lossless.as_slice(),
            None => &[],
        }
    }

    pub(crate) fn watches(&self, ext: usize) -> &[Arc<dyn WatchFactory>] {
        match self.extensions.get(ext) {
            Some(extension) => extension.watches.as_slice(),
            None => &[],
        }
    }

    pub(crate) fn watcher_owner(&self, watcher: &Arc<dyn WatchFactory>) -> Option<usize> {
        self.extensions.iter().position(|extension| {
            extension
                .watches
                .iter()
                .any(|registered| Arc::ptr_eq(registered, watcher))
        })
    }
}
