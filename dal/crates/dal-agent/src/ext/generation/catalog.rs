//! The generation-owned operation catalog (R03).
//!
//! One catalog per generation fixes the availability and input schema of
//! every native operation and the declaration of every scripted export.
//! Script adapters describe, check, and bind through it; they never parse
//! display text or keep a second registry.

use std::collections::BTreeMap;
use std::sync::Arc;

use dal_core::ext::{ExportId, NativeOp, OpId, OpSet};
use dal_core::{ModelId, ModelInfo, Name, Origin, RawJson, ToolSpec};

use super::super::Extension;
use super::super::tool::Tool;

/// One native operation's catalog entry (R03).
#[derive(Clone)]
pub struct OpSpec {
    /// The operation.
    pub op: NativeOp,
    /// Whether a backend exists; an unavailable operation fails with `unavailable`.
    pub available: bool,
    tool: Option<Arc<dyn Tool>>,
}

impl OpSpec {
    /// Returns the backing tool's input schema for `model`, when a tool backs the operation.
    ///
    /// The schema is captured per model because the patch dialect is.
    #[must_use]
    pub fn input(&self, model: &ModelInfo) -> Option<Arc<ToolSpec>> {
        self.tool.as_ref().map(|tool| tool.spec(model))
    }
}

/// One scripted export's declaration (R02 R03 R04).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportSpec {
    /// The export identity.
    pub id: ExportId,
    /// The declared primitive operations `D`.
    pub uses: OpSet,
    /// The normalized input schema.
    pub input: RawJson,
    /// The model-facing description.
    pub description: Box<str>,
}

/// Returns the provider-wire tool name of an export, `<plugin>__<local>` (R02).
///
/// Names are never truncated; a wire name that collides or exceeds the
/// name grammar rejects the generation.
#[must_use]
pub fn wire_name(id: &ExportId) -> String {
    format!("{}__{}", id.plugin, id.local)
}

/// Reports whether a host service backs a non-tool native operation today.
///
/// The match is exhaustive and fails closed: an operation whose owner has
/// not landed is `unavailable`, never a faked success (R03).
const fn host_backed(op: NativeOp) -> bool {
    match op {
        NativeOp::ModelsInfer
        | NativeOp::ModelsForward
        | NativeOp::NetFetch
        | NativeOp::AskConfirm
        | NativeOp::AskSelect
        | NativeOp::AskText
        | NativeOp::AgentsStart
        | NativeOp::AgentsWait
        | NativeOp::AgentsCancel
        | NativeOp::AgentsList
        | NativeOp::JobsStart
        | NativeOp::JobsCancel
        | NativeOp::JobsWait
        | NativeOp::JobsList
        | NativeOp::JobsText
        | NativeOp::TurnCancel
        | NativeOp::TurnSteer
        | NativeOp::TurnWake
        | NativeOp::TurnIsIdle
        | NativeOp::EnvRead => true,
        NativeOp::StateRead
        | NativeOp::StateWrite
        | NativeOp::StateDelete
        | NativeOp::ToolsRead
        | NativeOp::ToolsSearch
        | NativeOp::ToolsPatch
        | NativeOp::ToolsExec
        | NativeOp::McpCall => false,
    }
}

fn native_tool(extensions: &[Extension], op: NativeOp) -> Option<Arc<dyn Tool>> {
    let name = match op {
        NativeOp::ToolsRead => "read",
        NativeOp::ToolsSearch => "search",
        NativeOp::ToolsPatch => "patch",
        NativeOp::ToolsExec => "exec",
        _ => return None,
    };
    extensions
        .iter()
        .filter(|ext| ext.origin() != Origin::User)
        .flat_map(Extension::tools)
        .find(|(tool, _)| tool.name().as_str() == name)
        .map(|(tool, _)| Arc::clone(tool))
}

/// A scripted model export's route, handler location, and declared operations.
#[derive(Clone)]
pub(crate) struct ModelExport {
    /// The public model route.
    pub route: ModelId,
    /// The declared primitive operations.
    pub uses: OpSet,
    /// The owning extension index in the generation.
    pub extension: usize,
    /// The model record index within the extension.
    pub record: usize,
}

#[derive(Clone)]
pub(crate) struct ModelRouteEntry {
    /// The owning extension index in the generation.
    pub extension: usize,
    /// The model record index within the extension.
    pub record: usize,
}

/// The immutable operation catalog of one generation (R03).
#[derive(Clone)]
pub struct Catalog {
    native: [OpSpec; 28],
    exports: BTreeMap<ExportId, (ExportSpec, Name)>,
    model_routes: BTreeMap<Box<str>, ModelRouteEntry>,
    model_exports: BTreeMap<ExportId, ModelExport>,
}

impl Catalog {
    /// Builds a catalog for a test host over `extensions`, with no MCP
    /// backend; mirrors the generation build.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn for_test(extensions: &[Extension]) -> Self {
        Self::build(extensions, false)
    }

    /// Builds the catalog from canonical-order extensions (R03).
    ///
    /// Only product tools back `tools.*`: a user plugin tool that shares a
    /// native tool name never becomes a native operation.
    pub(crate) fn build(extensions: &[Extension], mcp: bool) -> Self {
        let native = NativeOp::ALL.map(|op| {
            let tool = native_tool(extensions, op);
            let available = match op {
                NativeOp::ToolsRead
                | NativeOp::ToolsSearch
                | NativeOp::ToolsPatch
                | NativeOp::ToolsExec => tool.is_some(),
                NativeOp::McpCall => mcp,
                other => host_backed(other),
            };
            OpSpec {
                op,
                available,
                tool,
            }
        });
        let exports = extensions
            .iter()
            .flat_map(Extension::exports)
            .map(|(spec, wire)| (spec.id.clone(), (spec.clone(), wire.clone())))
            .collect();
        let mut model_routes = BTreeMap::new();
        let mut model_exports = BTreeMap::new();
        for (extension, ext) in extensions.iter().enumerate() {
            for (record, model) in ext.models().iter().enumerate() {
                model_routes.insert(
                    model.id.as_str().into(),
                    ModelRouteEntry { extension, record },
                );
                if let Some(id) = &model.export {
                    model_exports.insert(
                        id.clone(),
                        ModelExport {
                            route: model.id.clone(),
                            uses: model.handler.uses(),
                            extension,
                            record,
                        },
                    );
                }
            }
        }
        Self {
            native,
            exports,
            model_routes,
            model_exports,
        }
    }

    /// Borrows the entry of one native operation.
    #[must_use]
    pub fn native(&self, op: NativeOp) -> &OpSpec {
        &self.native[op as usize]
    }

    /// Returns the product tool name that backs `op`, when one does (R03).
    ///
    /// The same products-only resolution as [`Catalog::build`]: a user
    /// plugin tool sharing a native name never backs the operation.
    #[must_use]
    pub fn native_tool_name(&self, op: NativeOp) -> Option<Name> {
        if !self.native(op).available {
            return None;
        }
        let name = match op {
            NativeOp::ToolsRead => "read",
            NativeOp::ToolsSearch => "search",
            NativeOp::ToolsPatch => "patch",
            NativeOp::ToolsExec => "exec",
            _ => return None,
        };
        Name::parse(name).ok()
    }

    /// Borrows the declaration of one export.
    #[must_use]
    pub fn export(&self, id: &ExportId) -> Option<&ExportSpec> {
        self.exports.get(id).map(|(spec, _)| spec)
    }

    /// Borrows one scripted model export.
    #[must_use]
    pub(crate) fn model_export(&self, id: &ExportId) -> Option<&ModelExport> {
        self.model_exports.get(id)
    }

    /// Looks up every registered model by its public route.
    #[must_use]
    pub(crate) fn model_route(&self, route: &str) -> Option<&ModelRouteEntry> {
        self.model_routes.get(route)
    }

    /// Borrows the provider-wire name of one export.
    #[must_use]
    pub fn wire(&self, id: &ExportId) -> Option<&Name> {
        self.exports.get(id).map(|(_, wire)| wire)
    }

    /// Resolves a provider-wire tool name back to its export.
    #[must_use]
    pub fn by_wire(&self, wire: &Name) -> Option<&ExportSpec> {
        self.exports
            .values()
            .find(|(_, name)| name == wire)
            .map(|(spec, _)| spec)
    }

    /// Reports whether `op` is known and available in this generation.
    #[must_use]
    pub fn available(&self, op: &OpId) -> bool {
        match op {
            OpId::Native(native) => self.native(*native).available,
            OpId::Export(id) => self.exports.contains_key(id),
        }
    }

    /// Iterates every export declaration in identity order.
    pub fn exports(&self) -> impl Iterator<Item = &ExportSpec> {
        self.exports.values().map(|(spec, _)| spec)
    }
}
