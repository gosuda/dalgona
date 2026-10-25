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
pub mod terminal;
mod tokens;
mod update;
mod view;
mod workspace;

pub use approval::{
    Decision, DenyReason, EXCLUSIVE_TOOLS, Gate, GrantSpec, PlannedCall, Policy, Rung, ToolClass,
    Unit, gate, headless_denial_text, parse_headless_denial, plan, rung,
};
pub use command::{
    BusyState, CancelScope, Chooser, Classify, Command, CommandError, Completion, ErrorTriple,
    Expect, ExportFormat, FrontAction, ImportFailure, LexError, Output, Rejection, Reply, Save,
    classify, tokens,
};
pub use config::{
    AgentsConfig, ApprovalMode, AskConfig, AskTimeoutClamp, ClampSide, Config, ConfigError,
    ConfigOverrides, ConfigProduct, EditStyleInput, GuardBands, GuardCheckMode, GuardPolicies,
    GuardSection, JudgeMode, Mode, ModeProjection, PROMPT_DIAGRAMS, PluginLimits, ProductSetting,
    RulesConfig, Screen, ServeConfig, SettingControl, SettingEdit, SettingValue, TuiConfig,
    edit_user_toml,
};
pub use content::{ContentError, ContentLimits, Part};
pub use ext::{
    AgentInfo, AgentRefusal, AgentReport, AgentStart, AgentState, AgentsOp, AgentsOpError,
    AgentsReply, ArtifactFile, Budget, Capability, Channel, Claimant, CommandName, CommandSpec,
    Consumer, ExitStatusKind, ExportId, ExportKind, FetchMethod, FetchRequest, FetchResponse,
    FindEntry, FindPage, HandleStatus, HookEvent, HookMismatch, HookOutcome, HookVerdict,
    InputEvent, InputVerdict, InterruptMode, JobCounts, JobEndEvent, JobEnds, JobLine, JobLines,
    JobReport, JobStateView, JobStatus, JobsError, JobsOp, JobsReply, MailMode, McpBlock,
    McpBlockError, McpDeclaration, McpEndpoint, McpLaunch, McpRequest, McpResponse, McpServerDecl,
    McpTargetError, ModelId, Name, NameError, NativeOp, OnError, OpId, OpSet, Origin, PluginSource,
    PrivateEffect, Provenance, RUST_STREAM_EVENT, ReadView, Receipt, RedirectPolicy,
    RegistrationError, RepeatMode, Revision, RuleFile, RuleRecord, RunOutput, RunRequest,
    RunRequestError, STAR_EVENTS, Scope, ScopeSpec, ScopeSpecError, ScopeUsage, SearchHit,
    SearchPage, ServerShapeError, Service, ServiceSet, SessionEnd, SessionStart, Settled,
    SidecarName, SidecarNameError, SidecarOp, Site, SkillRecord, SourceRow, StateError, StateKey,
    StateKeyError, StateNs, StateOp, StateRecord, StreamVerdict, SymbolHit, SymbolPage,
    ToolCallEvent, ToolCallVerdict, ToolData, ToolResultEvent, ToolSpec, TurnOp, TurnOpReply,
    UsesError, ViewNode, Visibility, WakeError, valid_tool_parameters, valid_version,
    validate_block,
};
pub use fold::{
    CompactLimits, CompactionExtRecord, CompactionReason, CompactionSummary, DeclaredFold, Effect,
    Emit, Event, LeafExt, Limits, ModelRequestPlan, PartialResponse, PendingCall, Phase,
    PrivateConflict, PrivateFact, PrivateTerminal, ReplayError, ResolveError, ResolvedCall,
    Session, Settings, SettledOutcome, Step, TurnSource, TurnStage,
};
pub use id::{
    BlobId, CallId, ClientId, EntryId, Gen, GenerationId, IdError, JobId, PrivateEffectId,
    PrivateLoopId, RequestId, Seq, SessionId, TurnId, resume_token,
};
pub use jiff::Timestamp;
pub use journal::{
    AssistantRoute, AssistantStop, BEFORE_TURN_SOURCE, Block, Branch, BranchError, BranchMode,
    DecodeError, Decoded, EncodeError, Entry, EntryKind, Header, InferredPurpose, JobEvent,
    JobKind, JobOutcome, JournalPart, MAX_RECORD, Mail, PrivateOutcome, Product, Record,
    ScannedHead, Source, TreeKind, TurnEndStop, TurnSequence, TurnSequenceError,
    VERSION as JOURNAL_VERSION, branch, decode, encode, scan_head,
};
pub use model::{
    AssistantPart, Caps, CompactOutcome, CompactedHistory, ContextItem, Family, InferFailure,
    Inference, MAX_SYNTHETIC_DEPTH, ModelInfo, ModelPrice, ModelRequest, ModelRoute, ModelToolSpec,
    PriceTier, Purpose, ReplayCapture, ReplayRefusal, ReplaySource, RequestParams, RouteError,
    Stop, StreamChannel, StreamEvent, ThinkingLevel, Usage, check_synthetic_chain,
};
pub use raw::{RawJson, RawJsonError};
pub use request::{
    Answer, AnswerValue, CallGrant, Choice, JobEnd, Owner, Preview, Question, Request,
};
pub use tokens::{estimate_text_tokens, estimate_tokens};
pub use update::{
    ExtState, ExtStatus, Notice, ResolutionCause, ToolOutcomeView, TurnCause, Update, UpdateKind,
};
pub use view::{
    AutoCompaction, EntryView, FileChange, ListQuery, Page, PageReq, PageReqError, SessionInfo,
    SessionSummary, SettingsView, Stats, TreeBranch, TreeDelta, TreeOutline, TurnState, UsageView,
    View,
};
pub use workspace::{Workspace, WorkspaceError};
