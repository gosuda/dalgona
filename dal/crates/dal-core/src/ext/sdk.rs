//! The fixed v1 operation vocabulary shared by the host, the script bridge, and
//! the generation catalog (R02 R03 R04 P05 E01 E05).
//!
//! Every script-visible operation has exactly one [`OpId`]. [`OpSet`] is the
//! set type used for `uses` declarations and invocation ceilings; it rejects
//! unknown ids, duplicates, and wildcards. [`Phase`] is the closed phase
//! allowlist that decides which operations a caller kind may ever request.

use std::fmt;

use super::{HookEvent, Name, Service, ServiceSet};

/// A native operation of the fixed v1 surface (R03).
///
/// The list is closed: adding an operation is a spec change, not a plugin
/// declaration. [`NativeOp::as_str`] is the exact `uses` spelling.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum NativeOp {
    /// Read file text through the host read owner.
    ToolsRead,
    /// Search file text through the host search owner.
    ToolsSearch,
    /// Apply a captured-dialect patch through the host patch owner.
    ToolsPatch,
    /// Run a process through the host exec owner.
    ToolsExec,
    /// Invoke one model inference turn.
    ModelsInfer,
    /// Forward one request inside an inference handler.
    ModelsForward,
    /// Perform one outbound HTTP request.
    NetFetch,
    /// Ask a yes/no question through the request broker.
    AskConfirm,
    /// Ask a single-choice question through the request broker.
    AskSelect,
    /// Ask a free-text question through the request broker.
    AskText,
    /// Read one state key from the caller namespace.
    StateRead,
    /// Compare-and-swap one state key in the caller namespace.
    StateWrite,
    /// Tombstone one state key in the caller namespace.
    StateDelete,
    /// Start a child agent session.
    AgentsStart,
    /// Await the report of a child agent session.
    AgentsWait,
    /// Cancel a child agent session.
    AgentsCancel,
    /// List the known child agent sessions.
    AgentsList,
    /// Start a background job.
    JobsStart,
    /// Await the outcome of a background job.
    JobsWait,
    /// Cancel a background job.
    JobsCancel,
    /// List the known background jobs.
    JobsList,
    /// Read the collected output text of a background job.
    JobsText,
    /// Cancel the active turn.
    TurnCancel,
    /// Add steering text to the active turn.
    TurnSteer,
    /// Wake the active turn.
    TurnWake,
    /// Report whether the active turn is idle.
    TurnIsIdle,
    /// Read one explicitly allowed environment key.
    EnvRead,
    /// Call one registered MCP tool.
    McpCall,
}

impl NativeOp {
    /// Every native operation, in catalog order.
    pub const ALL: [Self; 28] = [
        Self::ToolsRead,
        Self::ToolsSearch,
        Self::ToolsPatch,
        Self::ToolsExec,
        Self::ModelsInfer,
        Self::ModelsForward,
        Self::NetFetch,
        Self::AskConfirm,
        Self::AskSelect,
        Self::AskText,
        Self::StateRead,
        Self::StateWrite,
        Self::StateDelete,
        Self::AgentsStart,
        Self::AgentsWait,
        Self::AgentsCancel,
        Self::AgentsList,
        Self::JobsStart,
        Self::JobsWait,
        Self::JobsCancel,
        Self::JobsList,
        Self::JobsText,
        Self::TurnCancel,
        Self::TurnSteer,
        Self::TurnWake,
        Self::TurnIsIdle,
        Self::EnvRead,
        Self::McpCall,
    ];

    /// Returns the operation's exact `uses` id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolsRead => "tools.read",
            Self::ToolsSearch => "tools.search",
            Self::ToolsPatch => "tools.patch",
            Self::ToolsExec => "tools.exec",
            Self::ModelsInfer => "models.infer",
            Self::ModelsForward => "models.forward",
            Self::NetFetch => "net.fetch",
            Self::AskConfirm => "ask.confirm",
            Self::AskSelect => "ask.select",
            Self::AskText => "ask.text",
            Self::StateRead => "state.read",
            Self::StateWrite => "state.write",
            Self::StateDelete => "state.delete",
            Self::AgentsStart => "agents.start",
            Self::AgentsWait => "agents.wait",
            Self::AgentsCancel => "agents.cancel",
            Self::AgentsList => "agents.list",
            Self::JobsStart => "jobs.start",
            Self::JobsWait => "jobs.wait",
            Self::JobsCancel => "jobs.cancel",
            Self::JobsList => "jobs.list",
            Self::JobsText => "jobs.text",
            Self::TurnCancel => "turn.cancel",
            Self::TurnSteer => "turn.steer",
            Self::TurnWake => "turn.wake",
            Self::TurnIsIdle => "turn.is_idle",
            Self::EnvRead => "env.read",
            Self::McpCall => "mcp.call",
        }
    }

    /// Parses an exact `uses` id.
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.as_str() == id)
    }

    /// Returns the service the operation injects, if any (R03).
    ///
    /// Every operation named in `uses` contributes its service bit to the
    /// caller's injection set. Ask maps to [`Service::Ask`] even though it is
    /// capability-free: [`Service::capability`] strips it from grant keys, and
    /// the service layer admits capability-free services without a grant.
    #[must_use]
    pub const fn service(self) -> Option<Service> {
        match self {
            Self::ToolsRead | Self::ToolsSearch => Some(Service::FsRead),
            Self::ToolsPatch => Some(Service::FsWrite),
            Self::ToolsExec => Some(Service::Run),
            Self::ModelsInfer | Self::ModelsForward => Some(Service::Infer),
            Self::NetFetch => Some(Service::Net),
            Self::AskConfirm | Self::AskSelect | Self::AskText => Some(Service::Ask),
            Self::StateRead | Self::StateWrite | Self::StateDelete => Some(Service::Sidecar),
            Self::AgentsStart | Self::AgentsWait | Self::AgentsCancel | Self::AgentsList => {
                Some(Service::Agents)
            }
            Self::JobsStart
            | Self::JobsWait
            | Self::JobsCancel
            | Self::JobsList
            | Self::JobsText => Some(Service::Jobs),
            Self::TurnCancel | Self::TurnSteer | Self::TurnWake | Self::TurnIsIdle => {
                Some(Service::Turn)
            }
            Self::EnvRead => Some(Service::Env),
            Self::McpCall => Some(Service::Mcp),
        }
    }

    /// Whether the operation may be scheduled in a scope (E05).
    #[must_use]
    pub const fn schedulable(self) -> bool {
        matches!(
            self,
            Self::ToolsRead
                | Self::ToolsSearch
                | Self::ToolsPatch
                | Self::ToolsExec
                | Self::ModelsInfer
                | Self::NetFetch
        )
    }

    /// Returns the single positional convenience field of the built-in
    /// adapter, if one exists (R03).
    #[must_use]
    pub const fn positional(self) -> Option<&'static str> {
        match self {
            Self::ToolsRead => Some("path"),
            Self::ToolsSearch => Some("pattern"),
            Self::ToolsExec => Some("command"),
            _ => None,
        }
    }

    /// Returns the omitted-keyword defaults of the built-in adapter (R03).
    #[must_use]
    pub const fn sdk_defaults(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::ToolsSearch => &[("mode", "grep"), ("path", ".")],
            _ => &[],
        }
    }
}

impl fmt::Display for NativeOp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The kind of one exported extension entry (R02).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ExportKind {
    /// A model-callable tool.
    Tool,
    /// A user-invokable command.
    Command,
    /// A lifecycle or guarding hook.
    Hook,
    /// A scripted model route.
    Model,
}

impl ExportKind {
    /// Returns the plural word used in diagnostic spellings.
    ///
    /// Only tool and model exports are operations with `uses` ids; command
    /// and hook exports never appear in an [`OpId`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tools",
            Self::Command => "commands",
            Self::Hook => "hooks",
            Self::Model => "models",
        }
    }
}

/// The immutable identity of one exported entry (R02).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExportId {
    /// The plugin that declares the entry.
    pub plugin: Name,
    /// The entry kind.
    pub kind: ExportKind,
    /// The plugin-local entry name.
    pub local: Name,
}

/// One script-visible operation: a native operation or a composite export (R02 R03).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum OpId {
    /// A native operation of the fixed v1 surface.
    Native(NativeOp),
    /// A composite export entry; an entry permission, not a service (R03).
    Export(ExportId),
}

impl OpId {
    /// Parses a `uses` id.
    ///
    /// Accepts native ids such as `tools.read`, composite tool exports
    /// spelled `tools.<plugin>.<local>`, and scripted model routes spelled
    /// `models.<plugin>.<local>`.
    ///
    /// # Errors
    /// Returns [`UsesError::Wildcard`] when the id contains `*` and
    /// [`UsesError::Unknown`] for any other unparseable spelling.
    pub fn parse(id: &str) -> Result<Self, UsesError> {
        if id.contains('*') {
            return Err(UsesError::Wildcard { id: id.into() });
        }
        if let Some(native) = NativeOp::parse(id) {
            return Ok(Self::Native(native));
        }
        let malformed = || UsesError::Unknown { id: id.into() };
        let mut parts = id.split('.');
        let Some(head) = parts.next() else {
            return Err(malformed());
        };
        let kind = match head {
            "tools" => ExportKind::Tool,
            "models" => ExportKind::Model,
            _ => return Err(malformed()),
        };
        let Some(plugin) = parts.next() else {
            return Err(malformed());
        };
        let Some(local) = parts.next() else {
            return Err(malformed());
        };
        if parts.next().is_some() {
            return Err(malformed());
        }
        let Ok(plugin) = Name::parse(plugin) else {
            return Err(malformed());
        };
        let Ok(local) = Name::parse(local) else {
            return Err(malformed());
        };
        Ok(Self::Export(ExportId {
            plugin,
            kind,
            local,
        }))
    }
}

impl fmt::Display for OpId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(native) => formatter.write_str(native.as_str()),
            Self::Export(export) => write!(
                formatter,
                "{}.{}.{}",
                export.kind.as_str(),
                export.plugin,
                export.local
            ),
        }
    }
}

/// An error in a `uses` id list (R03 E01).
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum UsesError {
    /// The id is not a v1 operation.
    #[error("unknown operation \"{id}\"")]
    Unknown {
        /// The rejected id.
        id: Box<str>,
    },
    /// The id was already named in the same list.
    #[error("operation \"{id}\" appears twice in uses")]
    Duplicate {
        /// The duplicated id.
        id: Box<str>,
    },
    /// The id is a wildcard; wildcards are not accepted.
    #[error("operation \"{id}\" is a wildcard; uses takes exact ids")]
    Wildcard {
        /// The rejected id.
        id: Box<str>,
    },
    /// The list exceeds the `uses` cap.
    #[error("{count} uses entries exceeds the limit of 64")]
    TooMany {
        /// The rejected entry count.
        count: usize,
    },
}

/// A validated, duplicate-free set of operation ids (R03 E01 R04).
///
/// Native membership is a bitset; exports are kept sorted and deduplicated.
/// The default value is the empty set.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct OpSet {
    native: u32,
    exports: Vec<ExportId>,
}

impl OpSet {
    /// The empty set.
    pub const EMPTY: Self = Self {
        native: 0,
        exports: Vec::new(),
    };

    /// The maximum number of entries an explicit `uses` list may contain (R10).
    pub const MAX: usize = 64;

    /// Validates an explicit `uses` id list into a set.
    ///
    /// # Errors
    /// Returns [`UsesError::Unknown`] or [`UsesError::Wildcard`] for a bad
    /// id, [`UsesError::TooMany`] above [`OpSet::MAX`], and
    /// [`UsesError::Duplicate`] for a repeated id.
    pub fn parse<'a>(ids: impl IntoIterator<Item = &'a str>) -> Result<Self, UsesError> {
        let mut ops = Vec::new();
        for id in ids {
            ops.push(OpId::parse(id)?);
        }
        if ops.len() > Self::MAX {
            return Err(UsesError::TooMany { count: ops.len() });
        }
        let mut native = 0u32;
        let mut exports = Vec::new();
        for op in ops {
            match op {
                OpId::Native(op) => add_native(&mut native, op)?,
                OpId::Export(export) => add_export(&mut exports, export)?,
            }
        }
        Ok(Self { native, exports })
    }

    /// Reports whether the set contains the operation.
    #[must_use]
    pub fn contains(&self, op: &OpId) -> bool {
        match op {
            OpId::Native(native) => self.native & (1 << *native as u32) != 0,
            OpId::Export(export) => self.contains_export(export),
        }
    }

    /// Reports whether every operation of `self` is also in `of`.
    #[must_use]
    pub fn is_subset(&self, of: &Self) -> bool {
        self.native & !of.native == 0
            && self.exports.iter().all(|export| of.contains_export(export))
    }

    /// Returns the operations present in both sets.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            native: self.native & other.native,
            exports: self
                .exports
                .iter()
                .filter(|export| other.contains_export(export))
                .cloned()
                .collect(),
        }
    }

    /// Reports whether the set names no operation.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.native == 0 && self.exports.is_empty()
    }

    /// Returns the number of operations in the set.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.native.count_ones() as usize + self.exports.len()
    }

    /// Returns the union of the grant services the native members map to (R03).
    ///
    /// Composite exports are entry permissions, not services, and add none.
    #[must_use]
    pub fn services(&self) -> ServiceSet {
        let mut bits = 0u16;
        for op in NativeOp::ALL {
            if self.native & (1 << op as u32) == 0 {
                continue;
            }
            if let Some(service) = op.service() {
                bits |= service.mask();
            }
        }
        ServiceSet { bits }
    }

    /// Iterates every member, natives in catalog order and then exports in
    /// sorted order.
    pub fn iter(&self) -> impl Iterator<Item = OpId> + '_ {
        let native = NativeOp::ALL
            .into_iter()
            .filter(|op| self.native & (1 << *op as u32) != 0);
        let exports = self.exports.iter().cloned().map(OpId::Export);
        native.map(OpId::Native).chain(exports)
    }

    fn contains_export(&self, export: &ExportId) -> bool {
        self.exports.binary_search(export).is_ok()
    }
}

fn add_native(native: &mut u32, op: NativeOp) -> Result<(), UsesError> {
    let bit = 1 << op as u32;
    if *native & bit != 0 {
        return Err(UsesError::Duplicate {
            id: op.as_str().into(),
        });
    }
    *native |= bit;
    Ok(())
}

fn add_export(exports: &mut Vec<ExportId>, export: ExportId) -> Result<(), UsesError> {
    if exports.binary_search(&export).is_ok() {
        let id = OpId::Export(export).to_string();
        return Err(UsesError::Duplicate { id: id.into() });
    }
    let at = exports.partition_point(|known| known < &export);
    exports.insert(at, export);
    Ok(())
}

/// The caller kind of one invocation, and the operations it may request (R03 P05).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    /// A tool handler.
    Tool,
    /// A command handler.
    Command,
    /// An eval cell.
    Eval,
    /// An inference handler.
    Model,
    /// A hook handler with its closed P05 ceiling.
    Hook(HookEvent),
}

impl Phase {
    /// Reports whether the phase may ever request the operation.
    ///
    /// This is the closed phase allowlist P, one of the four authority sets
    /// of R04. It says nothing about `uses`, grants, or approval. Tool,
    /// command, eval, and model phases allow every declared operation except
    /// `models.forward`, which only an inference handler may call; the ban on
    /// export-to-export entry is an R04 entry rule, not part of this mask.
    #[must_use]
    pub fn permits(self, op: &OpId) -> bool {
        match (self, op) {
            (_, OpId::Native(NativeOp::ModelsForward)) => matches!(self, Self::Model),
            (Self::Tool | Self::Command | Self::Eval | Self::Model, _) => true,
            (Self::Hook(event), _) => permits_in_hook(event, op),
        }
    }
}

fn permits_in_hook(event: HookEvent, op: &OpId) -> bool {
    let OpId::Native(native) = op else {
        return false;
    };
    match event {
        HookEvent::SessionStart
        | HookEvent::SessionEnd
        | HookEvent::ToolResult
        | HookEvent::TurnEnd => {
            matches!(
                native,
                NativeOp::StateRead | NativeOp::StateWrite | NativeOp::StateDelete
            )
        }
        HookEvent::Settled => matches!(native, NativeOp::StateRead),
        HookEvent::Input
        | HookEvent::BeforeTurn
        | HookEvent::BeforeRequest
        | HookEvent::ToolCall => false,
    }
}

#[cfg(test)]
mod tests;
