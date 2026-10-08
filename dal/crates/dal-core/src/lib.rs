//! Domain values and state transitions shared by dal's execution surfaces.

pub mod approval;
pub mod command;
mod config;
mod content;
pub mod ext;
mod fold;
mod id;
mod journal;
mod model;
mod raw;
mod request;
mod tokens;
mod update;
mod view;
mod workspace;

pub use approval::{
    Decision, DenyReason, Gate, GrantSpec, PlannedCall, Policy, Rung, ToolClass, Unit, gate,
    headless_denial_text, parse_headless_denial, plan, rung,
};
pub use command::{
    BusyState, CancelScope, Chooser, Classify, Command, CommandError, Completion, ErrorTriple,
    Expect, ExportFormat, FrontAction, ImportFailure, LexError, Output, Rejection, Reply, Save,
    classify, tokens,
};
pub use config::{
    AgentsConfig, ApprovalMode, Config, ConfigError, ConfigOverrides, ConfigProduct,
    EditStyleInput, GuardBands, GuardCheckMode, GuardPolicies, GuardSection, JudgeMode, Mode,
    ModeProjection, PROMPT_DIAGRAMS, PluginLimits, RulesConfig, Screen, ServeConfig, TuiConfig,
};
pub use content::{ContentError, ContentLimits, Part};
pub use ext::{
    AgentInfo, AgentReport, AgentStart, AgentState, AgentsOp, AgentsOpError, AgentsReply,
    ArtifactFile, Budget, Capability, Channel, Claimant, CommandName, CommandSpec, Consumer,
    ExitStatusKind, ExportId, ExportKind, FetchMethod, FetchRequest, FetchResponse, FindEntry,
    FindPage, HandleStatus, HookEvent, HookMismatch, HookOutcome, HookVerdict, InputEvent,
    InputVerdict, InterruptMode, JobCounts, JobEndEvent, JobEnds, JobLine, JobLines, JobReport,
    JobStateView, JobStatus, JobsError, JobsOp, JobsReply, MailMode, McpRequest, McpResponse,
    ModelId, Name, NameError, NativeOp, OnError, OpId, OpSet, Origin, PluginSource, Provenance,
    RUST_STREAM_EVENT, ReadView, Receipt, RegistrationError, RepeatMode, Revision, RuleFile,
    RuleRecord, RunOutput, RunRequest, RunRequestError, STAR_EVENTS, Scope, ScopeSpec,
    ScopeSpecError, ScopeUsage, SearchHit, SearchPage, Service, ServiceSet, SessionEnd,
    SessionStart, Settled, SidecarOp, Site, SkillRecord, SourceRow, StateError, StateKey,
    StateKeyError, StateNs, StateOp, StateRecord, StreamVerdict, SymbolHit, SymbolPage,
    ToolCallEvent, ToolCallVerdict, ToolData, ToolResultEvent, ToolSpec, TurnOp, TurnOpReply,
    UsesError, ViewNode, Visibility, WakeError, valid_tool_parameters, valid_version,
};
pub use fold::{
    CompactLimits, CompactionExtRecord, CompactionReason, CompactionSummary, Effect, Emit, Event,
    LeafExt, Limits, ModelRequestPlan, PartialResponse, PendingCall, Phase, ReplayError,
    ResolveError, ResolvedCall, Session, Settings, SettledOutcome, Step, TurnSource, TurnStage,
};
pub use id::{
    BlobId, CallId, ClientId, EntryId, Gen, GenerationId, IdError, JobId, RequestId, Seq,
    SessionId, TurnId,
};
pub use jiff::Timestamp;
pub use journal::{
    AssistantStop, Block, Branch, BranchError, BranchMode, DecodeError, Decoded, EncodeError,
    Entry, EntryKind, Header, InferredPurpose, JobEvent, JobKind, JobOutcome, JournalPart, Mail,
    Product, Record, ScannedHead, Source, TreeKind, TurnEndStop, VERSION as JOURNAL_VERSION,
    branch, decode, encode, scan_head,
};
pub use model::{
    AssistantPart, Caps, CompactOutcome, CompactedHistory, ContextItem, Family, InferFailure,
    Inference, MAX_SYNTHETIC_DEPTH, ModelInfo, ModelPrice, ModelRequest, ModelRoute, ModelToolSpec,
    Purpose, ReplaySource, RequestParams, RouteError, Stop, StreamChannel, StreamEvent,
    ThinkingLevel, Usage, check_synthetic_chain,
};
pub use raw::{RawJson, RawJsonError};
pub use request::{
    Answer, AnswerValue, CallGrant, Choice, JobEnd, Owner, Preview, Question, Request,
};
pub use tokens::{estimate_text_tokens, estimate_tokens};
pub use update::{ExtState, ExtStatus, Notice, ToolOutcomeView, TurnCause, Update, UpdateKind};
pub use view::{
    AutoCompaction, EntryView, FileChange, ListQuery, Page, PageReq, PageReqError, SessionInfo,
    SessionSummary, SettingsView, Stats, TreeBranch, TreeDelta, TreeOutline, TurnState, UsageView,
    View,
};
pub use workspace::{Workspace, WorkspaceError};
