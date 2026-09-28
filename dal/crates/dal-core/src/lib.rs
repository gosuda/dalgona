//! Domain values and state transitions shared by dal's execution surfaces.

pub mod approval;
mod command;
mod config;
mod content;
pub mod ext;
mod fold;
mod id;
mod journal;
mod model;
mod raw;
mod request;
mod update;
mod view;
mod workspace;

pub use approval::{
    Decision, DenyReason, Gate, GrantSpec, PlannedCall, Policy, Rung, ToolClass, Unit, gate, plan,
    rung,
};
pub use command::{CancelScope, Command, Expect, Rejection, Reply};
pub use config::{
    ApprovalMode, Config, ConfigError, ConfigOverrides, ConfigProduct, JudgeMode, Mode,
    ModeProjection, RulesConfig, Screen, ServeConfig,
};
pub use content::{ContentError, ContentLimits, Part};
pub use ext::{
    AgentStart, AgentsOp, AgentsOpError, AgentsReply, BeforeRequest, BeforeTurn, Budget,
    Capability, Channel, Claimant, CommandSpec, ExitStatusKind, HandleStatus, HookEvent,
    HookMismatch, HookOutcome, HookVerdict, InputEvent, InputVerdict, InterruptMode, JobStateView,
    JobStatus, JobsOp, JobsReply, MailMode, Name, OnError, Origin, RUST_STREAM_EVENT, Receipt,
    RegistrationError, RepeatMode, RuleRecord, RunOutput, RunRequest, STAR_EVENTS, Scope,
    ScopeSpec, ScopeSpecError, ScopeUsage, Service, ServiceSet, SessionEnd, SessionStart, Settled,
    SidecarOp, Site, SkillRecord, StreamVerdict, ToolCallEvent, ToolCallVerdict, ToolResultEvent,
    ToolSpec, TurnOp, TurnOpReply, Visibility, valid_tool_parameters, valid_version,
};
pub use fold::{
    CompactionLimits, CompactionReason, CompactionSummary, Effect, Emit, Event, Limits,
    ModelRequestPlan, PartialResponse, PendingCall, Phase, ReplayError, ResolveError, ResolvedCall,
    Session, Settings, SettledOutcome, Step, TurnSource, TurnStage,
};
pub use id::{
    BlobId, CallId, ClientId, EntryId, Gen, GenerationId, IdError, JobId, RequestId, Seq,
    SessionId, TurnId,
};
pub use journal::{
    AssistantStop, Block, Branch, BranchError, BranchMode, DecodeError, Decoded, EncodeError,
    Entry, EntryKind, Header, InferredPurpose, JobEvent, JobKind, JobOutcome, JournalPart, Mail,
    Product, Record, ScannedHead, Source, TreeKind, TurnEndStop, VERSION as JOURNAL_VERSION,
    branch, decode, encode, scan_head,
};
pub use model::{
    AssistantPart, Caps, ContextItem, Family, InferFailure, Inference, MAX_SYNTHETIC_DEPTH,
    ModelInfo, ModelPrice, ModelRequest, ModelRoute, ModelToolSpec, Purpose, ReplaySource,
    RequestParams, RouteError, Stop, StreamChannel, StreamEvent, ThinkingLevel, Usage,
    check_synthetic_chain,
};
pub use raw::{RawJson, RawJsonError};
pub use request::{Answer, CallGrant, Choice, JobEnd, Owner, Preview, Question, Request};
pub use update::{Notice, ToolOutcomeView, TurnCause, Update, UpdateKind};
pub use view::{
    AutoCompaction, EntryView, FileChange, ListQuery, Page, PageReq, PageReqError, SessionInfo,
    SettingsView, Stats, TreeBranch, TreeDelta, TreeOutline, TurnState, UsageView, View,
};
pub use workspace::{Workspace, WorkspaceError};
