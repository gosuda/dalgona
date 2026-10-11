//! Versioned journal records, their strict line codec, and the pure
//! session-branch projection.
//!
//! `"v":1` and `"type"`. Encoding writes the declared member order so the
//! canonical vectors round-trip byte for byte. Decoding checks `"v"`
//! before `"type"` and the payload, drops record members this format does
//! not declare, and keeps the closed nested payloads strict.
//!
//! Durable member shapes follow the store's format-1 table rather than
//! the request wire: `who` and `purpose` use externally tagged objects,
//! `answer` is a bare literal or `{"value": ...}`, content parts carry
//! `base64` or `blob` spellings, `event` is one literal string, and the
//! stop members use their journal literals.
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
use crate::id::{CallId, ClientId, EntryId, Gen, JobId, RequestId, SessionId, TurnId};
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
mod types;
mod wire;
mod wire_entries;

pub use branch::{Branch, BranchError, BranchMode, branch};
pub use decode_records::{decode, scan_head};
pub use encode::encode;
pub use entries::{Entry, InferredPurpose, JobEvent, JobKind, JobOutcome, Mail};
pub use records::{DecodeError, Decoded, EncodeError, Record, ScannedHead, TreeKind};
pub use types::{
    AssistantStop, Block, EntryKind, Header, JournalPart, Product, Source, TurnEndStop, VERSION,
};

#[cfg(test)]
mod tests;
