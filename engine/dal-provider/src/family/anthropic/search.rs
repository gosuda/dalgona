//! Anthropic's native tool search: the models that take it, the server tool a
//! request carries, and the replay of the blocks it streams back.
//!
//! A request that defers tools carries one [`ServerTool`] plus every deferred
//! tool with `defer_loading: true`. The response then holds a
//! `server_tool_use` block and its `tool_search_tool_result`, whose
//! `tool_reference` members name the tools the search found. Both blocks go
//! back verbatim, as one pair, so the API keeps expanding the references on
//! later turns. Decoding is tolerant: members this module does not name are
//! ignored, and they still replay because the stored block is the raw text.

use std::collections::HashSet;

use dal_core::RawJson;
use serde::{Deserialize, Serialize};
use sonic_rs::LazyValue;

use super::protocol;
use crate::error::ProviderError;

/// The server tool type of Anthropic's BM25 tool search.
pub(super) const TOOL_TYPE: &str = "tool_search_tool_bm25_20251119";

/// The server tool's name; Anthropic fixes it.
pub(super) const TOOL_NAME: &str = "tool_search_tool_bm25";

const CALL_TYPE: &str = "server_tool_use";
const RESULT_TYPE: &str = "tool_search_tool_result";

/// A trailing group of this many digits in a model id is a release date, not
/// a minor version.
const DATE_SUFFIX_LEN: usize = 8;

/// Whether `model` on `provider` accepts the native search server tool and
/// `tool_reference` blocks.
///
/// Only the first-party `anthropic` provider qualifies: gateways that speak
/// the Messages API reject the server tool. Models are Opus, Sonnet, Fable,
/// and Mythos from 4.5 on; Opus 4.1 and earlier reject the server tool, and
/// Haiku stays off because it rejects `tool_reference` blocks. A model that
/// fails this test gets the client `tool_search` tool instead.
#[must_use]
pub fn supports_tool_search(provider: &str, model: &str) -> bool {
    if provider != "anthropic" {
        return false;
    }
    let Some(rest) = model.strip_prefix("claude-") else {
        return false;
    };
    let mut parts = rest.split('-');
    if !matches!(parts.next(), Some("opus" | "sonnet" | "fable" | "mythos")) {
        return false;
    }
    let Some(major) = parts.next().and_then(|digits| digits.parse::<u32>().ok()) else {
        return false;
    };
    let minor = parts
        .next()
        .filter(|digits| digits.len() < DATE_SUFFIX_LEN)
        .and_then(|digits| digits.parse::<u32>().ok())
        .unwrap_or(0);
    major > 4 || (major == 4 && minor >= 5)
}

/// The server tool entry of the `tools` array.
#[derive(Serialize)]
pub(super) struct ServerTool {
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'static str,
}

/// The one server tool every deferring request carries.
pub(super) const SERVER_TOOL: ServerTool = ServerTool {
    kind: TOOL_TYPE,
    name: TOOL_NAME,
};

/// Stores the replay blocks that sat next to each other in one response as
/// one payload: the block itself when alone, else a JSON array of them.
pub(super) fn group(blocks: Vec<RawJson>) -> Result<RawJson, ProviderError> {
    let blocks = match <[RawJson; 1]>::try_from(blocks) {
        Ok([only]) => return Ok(only),
        Err(blocks) => blocks,
    };
    let joined = blocks
        .iter()
        .map(RawJson::as_str)
        .collect::<Vec<_>>()
        .join(",");
    RawJson::parse(&format!("[{joined}]"))
        .map_err(|error| protocol(format!("replay group did not encode: {error}")))
}

/// Whether a stored replay payload is a group of blocks.
pub(super) fn is_group(raw: &RawJson) -> bool {
    raw.as_str().starts_with('[')
}

/// The blocks of a stored group, or `None` when the group is malformed.
pub(super) fn members(raw: &RawJson) -> Option<Vec<RawJson>> {
    let blocks: Vec<LazyValue<'_>> = sonic_rs::from_str(raw.as_str()).ok()?;
    blocks
        .iter()
        .map(|block| RawJson::parse(block.as_raw_str()).ok())
        .collect()
}

/// The replay of a finished `server_tool_use` block: the block as the stream
/// opened it, member for member in its own order, with the arguments its
/// deltas assembled in place of the opening `input`. That `input` stands when
/// no delta came. `None` drops a block that cannot be rebuilt.
pub(super) fn call_replay(start: &RawJson, args: &[u8]) -> Option<RawJson> {
    let args = std::str::from_utf8(args).ok()?.trim();
    let mut members = Vec::new();
    for member in sonic_rs::to_object_iter(start.as_str()) {
        let (key, value) = member.ok()?;
        let raw = if key == "input" && !args.is_empty() {
            args
        } else {
            value.as_raw_str()
        };
        members.push(format!("{}:{raw}", sonic_rs::to_string(&key).ok()?));
    }
    RawJson::parse(&format!("{{{}}}", members.join(","))).ok()
}

/// The members of a stored block this module reads; every other member is
/// ignored.
#[derive(Deserialize)]
struct Wire {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    name: Option<String>,
    tool_use_id: Option<String>,
    content: Option<Content>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(default)]
    tool_references: Vec<Reference>,
}

#[derive(Deserialize)]
struct Reference {
    tool_name: Option<String>,
}

/// What a stored block is to the search replay.
enum Kind {
    /// A search call, by its id.
    Call(String),
    /// A search result: the call it answers and the tools it names.
    Result { call: String, tools: Vec<String> },
    /// Anything else, including a search block this module cannot read.
    Other,
}

fn kind(raw: &RawJson) -> Kind {
    let Ok(block) = sonic_rs::from_str::<Wire>(raw.as_str()) else {
        return Kind::Other;
    };
    match (block.kind.as_deref(), block.id, block.tool_use_id) {
        (Some(CALL_TYPE), Some(id), _) if block.name.as_deref() == Some(TOOL_NAME) => {
            Kind::Call(id)
        }
        (Some(RESULT_TYPE), _, Some(call)) => Kind::Result {
            call,
            tools: block
                .content
                .into_iter()
                .flat_map(|content| content.tool_references)
                .filter_map(|reference| reference.tool_name)
                .collect(),
        },
        _ => Kind::Other,
    }
}

/// One replayable piece of a stored group.
pub(super) enum Piece {
    /// A search call with its result.
    Pair(RawJson, RawJson),
    /// Any other block, for the caller's own rules.
    Other(RawJson),
}

/// Splits a stored group into pieces.
///
/// A search call replays only with its result, and only when the request
/// carries the search tool (`tools` is `Some`) and every tool the result
/// names; otherwise both are dropped, because the API rejects a reference to
/// a tool the request no longer lists. A result without its call is dropped.
pub(super) fn pieces(blocks: Vec<RawJson>, tools: Option<&HashSet<&str>>) -> Vec<Piece> {
    let mut slots: Vec<Option<RawJson>> = blocks.into_iter().map(Some).collect();
    let mut pieces = Vec::new();
    for at in 0..slots.len() {
        let Some(block) = slots[at].take() else {
            continue;
        };
        match kind(&block) {
            Kind::Other => pieces.push(Piece::Other(block)),
            Kind::Result { .. } => {}
            Kind::Call(call) => {
                let Some((result, named)) = take_result(&mut slots[at + 1..], &call) else {
                    continue;
                };
                let listed = tools
                    .is_some_and(|listed| named.iter().all(|name| listed.contains(name.as_str())));
                if listed {
                    pieces.push(Piece::Pair(block, result));
                }
            }
        }
    }
    pieces
}

/// Takes the result that answers `call` out of `rest`, with the tool names it
/// carries.
fn take_result(rest: &mut [Option<RawJson>], call: &str) -> Option<(RawJson, Vec<String>)> {
    let (index, named) =
        rest.iter()
            .enumerate()
            .find_map(|(index, slot)| match kind(slot.as_ref()?) {
                Kind::Result {
                    call: answered,
                    tools,
                } if answered == call => Some((index, tools)),
                _ => None,
            })?;
    Some((rest[index].take()?, named))
}
