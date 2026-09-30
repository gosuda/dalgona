//! Plain values shared by extension registration, hooks, and runtime operations.

use std::{fmt, str::FromStr, sync::Arc, time::Duration};

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::approval::ToolClass;

use crate::content::Part;
use crate::id::{CallId, EntryId, JobId, SessionId, TurnId};
use crate::journal::JobOutcome;
use crate::model::{Caps, ModelInfo, RequestParams, Stop};
use crate::raw::{RawJson, Tagged};
use crate::workspace::Workspace;

mod hooks;
mod mcp;
mod names;
mod ops;
mod protocol;
mod scope;
mod scopes;
mod sdk;
mod services;
mod skill_front;
mod state;
mod tool_data;

pub use hooks::{HandleStatus, HookEvent, HookMismatch, HookOutcome, HookVerdict};
pub use mcp::{
    McpBlock, McpBlockError, McpDeclaration, McpServerDecl, ServerShapeError, validate_block,
};
pub use names::{CommandName, MAPPED_TOOL_NAME_MAX, ModelId, Name, NameError, Origin, Visibility};
pub use ops::{
    AgentInfo, AgentReport, AgentStart, AgentState, AgentsOp, AgentsOpError, ArtifactFile, JobsOp,
    Mail, MailMode, Receipt, SidecarOp, TurnOp,
};
pub use protocol::{
    AgentsReply, ExitStatusKind, FetchMethod, FetchRequest, FetchResponse, JobCounts, JobEndEvent,
    JobEnds, JobLine, JobLines, JobReport, JobStateView, JobStatus, JobsError, JobsReply,
    McpRequest, McpResponse, RunOutput, RunRequest, RunRequestError, TurnOpReply, WakeError,
};
pub use scope::{Budget, OnError, ScopeSpec, ScopeSpecError, ScopeUsage};
pub use scopes::{
    BeforeRequest, BeforeTurn, Channel, GateSeed, InputEvent, InputVerdict, InterruptMode,
    RUST_STREAM_EVENT, RepeatMode, RuleRecord, STAR_EVENTS, Scope, SessionEnd, SessionStart,
    Settled, StreamFire, StreamFireAction, StreamVerdict, ToolCallEvent, ToolCallVerdict,
    ToolResultEvent, TurnEnd, WatchBudget,
};
pub use sdk::{ExportId, ExportKind, NativeOp, OpId, OpSet, Phase, UsesError};
pub use services::{
    Capability, Claimant, CommandSpec, PluginSource, RegistrationError, RuleFile, Service,
    ServiceSet, Site, SkillRecord, ToolSpec, valid_tool_parameters, valid_version,
};
pub use skill_front::{Kind as SkillFrontKind, SkillFrontError, decode_skill_mcp};
pub use state::{Revision, StateError, StateKey, StateKeyError, StateNs, StateOp, StateRecord};
pub use tool_data::{
    Consumer, FindEntry, FindPage, Provenance, ReadView, SearchHit, SearchPage, SourceRow,
    SymbolHit, SymbolPage, ToolData, ViewNode,
};

#[cfg(test)]
mod tests;
