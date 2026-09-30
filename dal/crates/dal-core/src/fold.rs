//! Pure, deterministic session-state transitions and journal replay.
//!
//! The reducer owns protocol state only. Provider calls, clocks, I/O, locks,
//! extension execution, and request brokering remain effects of the actor.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use crate::approval::{PlannedCall, Policy, ToolClass, Unit, plan};
use crate::command::{CancelScope, Command, Expect, Output, Rejection, Reply};
use crate::config::{ApprovalMode, Mode};
use crate::content::Part;
use crate::ext::{HookEvent, HookOutcome, HookVerdict, Name, StreamVerdict, ToolCallVerdict};
use crate::id::{CallId, ClientId, EntryId, Gen, JobId, RequestId, TurnId};
use crate::journal::{
    AssistantStop, Block, DecodeError, Entry, EntryKind, JobEvent, JobKind, JobOutcome,
    JournalPart, Record, TurnEndStop,
};
use crate::model::{
    Family, InferFailure, Inference, ModelRoute, RequestParams, Stop, StreamChannel, StreamEvent,
    ThinkingLevel, Usage,
};
use crate::raw::RawJson;
use crate::request::{Answer, Question, Request};
use crate::update::{Notice, ToolOutcomeView, TurnCause, UpdateKind};
use crate::view::{EntryView, FileChange, SettingsView, TreeDelta, TurnState};

mod apply;
mod cancel;
mod ext;
mod helpers;
mod preflight;
mod replay;
mod round;
mod session;
mod stream;
mod turns;
mod types;
mod views;

pub use ext::LeafExt;
pub use session::Session;
pub use types::{
    CompactLimits, CompactionExtRecord, CompactionReason, CompactionSummary, Effect, Emit, Event,
    Limits, ModelRequestPlan, PartialResponse, PendingCall, Phase, ReplayError, ResolveError,
    ResolvedCall, Settings, SettledOutcome, Step, TurnSource, TurnStage,
};

#[cfg(test)]
mod tests;
