//! Versioned journal records, their strict line codec, and the pure
//! session-branch projection.
//! Existing record kinds retain format 1; turn reservations require format 2
//! and mail deliveries require format 3 (see [`Record::format`]).
//! Encoding uses the declared member order, so canonical vectors round-trip
//! byte for byte. Line length can be measured without allocating the line
//! via [`encoded_len`]. Decoding checks the version before the type and payload,
//! drops undeclared record members, and keeps closed nested payloads strict.
//!
//! Durable member shapes follow the store's format-1 table rather than
//! the request wire: `who` and `purpose` use externally tagged objects,
//! `answer` is a bare literal or `{"value": ...}`, content parts carry
//! `base64` or `blob` spellings, `event` is one literal string, and the
//! stop members use their journal literals. An assistant `route` is
//! `"synthetic"`, `"harness"`, or `{"api":<family>}`. Records that predate
//! route metadata omit this member.
//!
//! Provider replay payloads are raw JSON kept byte for byte. A `text` block
//! writes its payload as `replay`, or the historical `"replay":null` when it
//! has none. A `tool_call` block and an assistant record write `replay` as
//! their last member only when a payload exists, so records that predate it
//! keep their bytes. A present `null` assistant `replay` is malformed, like
//! a present `null` route.
//!
//! Money is the integer `cost_micro_usd` member of the usage object,
//! never a float on disk. [`Usage`] holds provider-reported dollars in
//! `cost_usd`, so the codec converts at this boundary: encoding rounds a
//! reported cost to micro-dollars, and rejects a non-finite, negative,
//! or unrepresentable cost with [`EncodeError::InvalidCost`] rather than
//! writing `null`, which the format reserves for "no cost was reported".

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

use crate::config::{ApprovalMode, Mode};
use crate::ext::MailMode;
use crate::id::{
    CallId, ClientId, EntryId, Gen, JobId, PrivateEffectId, PrivateLoopId, RequestId, SessionId,
    TurnId,
};
use crate::model::{Family, ModelRoute, RouteError, ThinkingLevel, Usage};
use crate::raw::{RawJson, Tagged};
use crate::request::{Answer, Owner};
use crate::view::FileChange;
use crate::workspace::Workspace;

mod branch;
mod decode;
mod decode_records;
mod encode;
mod entries;
mod records;
mod scan;
mod turn_sequence;
mod types;
mod wire;
mod wire_entries;

pub use branch::{Branch, BranchError, BranchMode, branch};
pub use decode_records::{decode, scan_head};
pub use encode::encode;
pub(crate) use encode::encoded_len;
pub use entries::{DeliveredMail, Entry, InferredPurpose, JobEvent, JobKind, JobOutcome, Mail};
pub use records::{
    DecodeError, Decoded, EncodeError, PrivateOutcome, Record, ScannedHead, TreeKind,
};
pub use turn_sequence::{TurnSequence, TurnSequenceError};
pub use types::{
    AssistantRoute, AssistantStop, BEFORE_TURN_SOURCE, Block, EntryKind, Header, JournalPart,
    Product, Source, TurnEndStop, VERSION,
};

/// The largest one encoded journal line may be, in bytes, line feed included.
pub const MAX_RECORD: u64 = 67_108_864;

#[cfg(test)]
mod tests;
