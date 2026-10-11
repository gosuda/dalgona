//! The one owner of provider-safe tool names.
//!
//! An internal tool name may hold dots and run to 200 bytes (a mapped MCP
//! tool is `<skill>.<server>.<tool>`); `OpenAI` and Anthropic accept only
//! `[A-Za-z0-9_-]{1,64}` as a function name. [`wire_name`] maps every
//! internal name to a valid wire name and every family encoder calls it, so
//! request bodies never carry an invalid name. [`ToolNames`] is the
//! per-request table from wire name back to internal name; it rejects two
//! advertised tools that share a wire name and restores the internal name in
//! the tool calls of the response stream.

use std::{borrow::Cow, collections::HashMap};

use dal_core::ModelRequest;
use sha2::{Digest, Sha256};

use crate::{claude_fingerprint::CLAUDE_TOOL_PREFIX, error::ProviderError, stream::StreamEvent};

/// The longest function name any family accepts, in bytes.
pub(crate) const WIRE_NAME_MAX: usize = 64;

const HASH_DIGITS: usize = 8;

/// The longest wire name before Anthropic OAuth adds its custom-tool prefix.
pub(crate) const ANTHROPIC_OAUTH_NAME_MAX: usize = WIRE_NAME_MAX - CLAUDE_TOOL_PREFIX.len();

const fn wire_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}
const fn hex_digit(value: u8) -> char {
    match value {
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'a',
        11 => 'b',
        12 => 'c',
        13 => 'd',
        14 => 'e',
        15 => 'f',
        _ => '0',
    }
}

/// Maps `internal` to a wire name of at most `max` bytes over
/// `[A-Za-z0-9_-]`.
///
/// A name that already fits is returned unchanged. Otherwise each character
/// outside the alphabet becomes `_`, the result is cut to leave room for the
/// suffix, and `_` plus 8 lowercase hex digits of the SHA-256 of the internal
/// name are appended. The mapping is a pure function of `internal` and `max`.
#[must_use]
pub(crate) fn wire_name(internal: &str, max: usize) -> Cow<'_, str> {
    if !internal.is_empty() && internal.len() <= max && internal.chars().all(wire_char) {
        return Cow::Borrowed(internal);
    }
    let keep = max.saturating_sub(HASH_DIGITS + 1);
    let mut name = String::with_capacity(keep + HASH_DIGITS + 1);
    name.extend(
        internal
            .chars()
            .take(keep)
            .map(|character| if wire_char(character) { character } else { '_' }),
    );
    name.push('_');
    for byte in &Sha256::digest(internal.as_bytes())[..HASH_DIGITS / 2] {
        name.push(hex_digit(byte >> 4));
        name.push(hex_digit(byte & 0xf));
    }
    Cow::Owned(name)
}

/// The wire-to-internal table of one request.
#[derive(Clone, Debug)]
pub(crate) struct ToolNames {
    internal: HashMap<Box<str>, Box<str>>,
}

impl ToolNames {
    /// Builds the table of the tools `request` advertises, mapped at `max`.
    ///
    /// # Errors
    /// [`ProviderError::ToolNameCollision`] when two different tools map to
    /// one wire name; it names the wire name and both internal names.
    pub(crate) fn for_request(request: &ModelRequest, max: usize) -> Result<Self, ProviderError> {
        let mut seen: HashMap<Cow<'_, str>, &str> = HashMap::with_capacity(request.tools.len());
        for tool in request.tools.iter() {
            let wire = wire_name(&tool.name, max);
            if let Some(first) = seen.get(wire.as_ref()) {
                if *first != tool.name.as_ref() {
                    return Err(ProviderError::ToolNameCollision {
                        wire: wire.to_string(),
                        first: (*first).to_owned(),
                        second: tool.name.to_string(),
                    });
                }
                continue;
            }
            seen.insert(wire, tool.name.as_ref());
        }
        let mut internal = HashMap::new();
        for (wire, name) in seen {
            if wire.as_ref() != name {
                internal.insert(wire.into_owned().into_boxed_str(), name.into());
            }
        }
        Ok(Self { internal })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.internal.is_empty()
    }

    fn restore(&self, name: String) -> String {
        match self.internal.get(name.as_str()) {
            Some(internal) => internal.to_string(),
            None => name,
        }
    }

    /// Rewrites the tool names of one stream event back to internal names.
    /// A name the request never advertised passes through unchanged.
    pub(crate) fn restore_event(&self, event: StreamEvent) -> StreamEvent {
        match event {
            StreamEvent::ToolCallStarted { id, name } => StreamEvent::ToolCallStarted {
                id,
                name: self.restore(name),
            },
            StreamEvent::ToolCallsDone { mut calls } => {
                for call in &mut calls {
                    call.name = self.restore(std::mem::take(&mut call.name));
                }
                StreamEvent::ToolCallsDone { calls }
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests;
