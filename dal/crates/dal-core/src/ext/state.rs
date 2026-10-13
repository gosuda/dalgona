//! Compare-and-swap state values shared by the state owner and the script
//! bridge (R08).
//!
//! State is explicit and session-scoped. A namespace isolates origin,
//! plugin identity, and `state_version`; eval has its own namespace. Every
//! write and delete names the [`Revision`] it expects, and revisions are
//! never reused, so a stale token can never revive a deleted key.

use std::{fmt, num::NonZeroU32, num::NonZeroU64};

use super::{Name, Origin, RawJson};

/// The namespace one state key lives in (R08).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum StateNs {
    /// A plugin namespace, isolated by origin, plugin, and `state_version`.
    Plugin {
        /// The plugin's origin class.
        origin: Origin,
        /// The plugin identity.
        plugin: Name,
        /// The plugin's declared `state_version`.
        version: NonZeroU32,
    },
    /// The session's eval namespace.
    Eval,
}

/// A checked state key with the grammar `[a-z][a-z0-9_.-]{0,63}` (R08).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StateKey(Box<str>);

impl StateKey {
    /// Parses one state key.
    ///
    /// # Errors
    /// Returns [`StateKeyError`] when the value is outside the key grammar.
    pub fn parse(value: &str) -> Result<Self, StateKeyError> {
        let mut bytes = value.bytes();
        let head = matches!(bytes.next(), Some(b'a'..=b'z'));
        let tail = bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
        });
        if head && tail && value.len() <= 64 {
            return Ok(Self(value.into()));
        }
        Err(StateKeyError { key: value.into() })
    }

    /// Returns the validated key text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StateKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A state key outside the R08 grammar.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid state key \"{key}\"; keys must match [a-z][a-z0-9_.-]{{0,63}}")]
pub struct StateKeyError {
    /// The rejected key.
    pub key: Box<str>,
}

/// An opaque state revision; the state owner never reuses one (R08).
///
/// Scripts can only compare and pass revisions back; they cannot build or
/// persist one.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Revision(NonZeroU64);

impl Revision {
    /// Wraps a revision number minted by the state owner.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// Returns the revision number.
    #[must_use]
    pub const fn get(self) -> NonZeroU64 {
        self.0
    }
}

/// One state operation on a single key (R08).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateOp {
    /// Reads a key; a missing key still mints a revision.
    Read {
        /// The namespace.
        ns: StateNs,
        /// The key.
        key: StateKey,
    },
    /// Writes a key when its current revision equals `expected`.
    Write {
        /// The namespace.
        ns: StateNs,
        /// The key.
        key: StateKey,
        /// The value to store.
        value: RawJson,
        /// The revision the caller last observed.
        expected: Revision,
    },
    /// Tombstones a key when its current revision equals `expected`.
    Delete {
        /// The namespace.
        ns: StateNs,
        /// The key.
        key: StateKey,
        /// The revision the caller last observed.
        expected: Revision,
    },
}

/// The state of one key after an operation (R08).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateRecord {
    /// Whether the key holds a value.
    pub present: bool,
    /// The value, only when present.
    pub value: Option<RawJson>,
    /// The key's current revision.
    pub revision: Revision,
}

/// An expected state operation failure (R08).
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StateError {
    /// The expected revision is stale; nothing was written.
    #[error("state revision conflict")]
    Conflict,
    /// The session has no durable state, such as an ephemeral session.
    #[error("state is unavailable in this session")]
    Unavailable,
}

#[cfg(test)]
mod tests;
