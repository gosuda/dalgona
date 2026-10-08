//! Provider-request assembly: model metadata, system prompt, tool list,
//! parameters, and stream-call resolution.
//!
//! The driver owns caching: session sections render once per generation,
//! and the caller supplies the render closure with its live services.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

use dal_core::{
    AssistantPart, Block, CallId, Caps, ContextItem, EntryKind, EntryView, Family, JournalPart,
    Mode, ModelId, ModelInfo, ModelRoute, ModelToolSpec, Name, Part, RawJson, ReplaySource,
    ResolveError, ResolvedCall, ThinkingLevel, ToolClass, Visibility, Workspace,
};
use dal_provider::{CatalogEntry, clamp, levels_for};

use crate::ext::generation::Generation;
use crate::ext::overlay::TurnTools;
use crate::ext::prompt::PromptSection;
const TOOL_SEARCH_NAME: &str = "tool_search";
const TOOL_SEARCH_PARAMETERS: &str = r#"{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}"#;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeferredTool {
    pub(crate) name: Box<str>,
    pub(crate) description: Box<str>,
    pub(crate) parameters: Box<str>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolSearchArgs {
    query: Box<str>,
}

/// Display metadata for one resolved catalog row.
pub(crate) fn model_info_for(
    entry: &CatalogEntry,
    route: &ModelRoute,
    default_level: ThinkingLevel,
) -> ModelInfo {
    let family = api_family(route).unwrap_or(Family::Chat);
    let (effective, _) = clamp(default_level, &entry.thinking);
    ModelInfo {
        route: route.clone(),
        name: format!("{} · {}/{}", entry.display, entry.provider, entry.id).into(),
        caps: Caps {
            context_window: entry.context_window,
            thinking: levels_for(&entry.thinking),
            tool_use: entry.tool_support.allows(family, effective),
            image_input: entry.image_input,
            custom_grammar: entry.custom_grammar
                && matches!(family, Family::Responses | Family::Codex),
        },
    }
}

/// The API family of one resolved route, when it names a provider API.
pub(crate) fn api_family(route: &ModelRoute) -> Option<Family> {
    match route {
        ModelRoute::Api { family, .. } => Some(*family),
        _ => None,
    }
}

/// Joins prompt sections byte-stable with one blank line.
///
/// Static sections contribute fixed bytes; session sections render through
/// the driver's closure, which caches per generation.
pub(crate) fn system_prompt(
    generation: &Generation,
    mode: Mode,
    render_session: &dyn Fn(&PromptSection) -> Option<String>,
) -> String {
    let mut sections: Vec<(u8, String)> = Vec::new();
    for section in generation.prompt_sections.iter() {
        if !section_visible(section.visibility(), mode) {
            continue;
        }
        let text = match section {
            PromptSection::Static { text, .. } => Some(text.to_string()),
            PromptSection::Session { .. } => render_session(section),
        };
        if let Some(text) = text {
            sections.push((section.order().position(), text));
        }
    }
    sections.sort_by_key(|(order, _)| *order);
    sections
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Whether a section visibility reaches the model in this mode.
fn section_visible(visibility: Visibility, mode: Mode) -> bool {
    matches!(
        (visibility, mode),
        (Visibility::Model, _) | (Visibility::EvalOnly, Mode::EvalFirst | Mode::EvalOnly)
    )
}

/// Orders model-visible tool specs byte by byte, eval-first in eval modes.
pub(crate) fn tool_list(
    generation: &Generation,
    turn_tools: &TurnTools,
    model: &ModelInfo,
    model_id: &ModelId,
    mode: Mode,
) -> (Vec<ModelToolSpec>, Arc<[DeferredTool]>) {
    let deferred = deferred_tool_catalog(generation, turn_tools, model, model_id);
    let mut specs: Vec<(bool, Arc<dal_core::ToolSpec>)> = Vec::new();
    for entry in generation.tools.entries() {
        let name = entry.name.clone();
        if !turn_tools.permits(&name) {
            continue;
        }
        let visible = generation
            .tool_visibility(&name)
            .map(|declared| turn_tools.effective(&name, declared));
        if !tool_visible(visible, mode) {
            continue;
        }
        if let Some(spec) = generation.tool_spec(&name, model, model_id) {
            let eval_first = matches!(visible, Some(Visibility::EvalOnly));
            specs.push((eval_first, spec));
        }
    }
    for entry in turn_tools.entries() {
        let visible = Some(turn_tools.effective(entry.tool.name(), entry.visibility));
        if tool_visible(visible, mode) {
            let eval_first = matches!(visible, Some(Visibility::EvalOnly));
            specs.push((eval_first, entry.tool.spec(model)));
        }
    }
    specs.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| spec_bytes(&left.1).cmp(&spec_bytes(&right.1)))
    });
    let mut tools: Vec<ModelToolSpec> = specs
        .into_iter()
        .map(|(_, spec)| ModelToolSpec {
            name: spec.name.as_str().into(),
            description: spec.description.clone(),
            parameters: spec.parameters.clone(),
            grammar: spec.grammar.clone(),
        })
        .collect();
    if !deferred.is_empty() && !has_registered_tool_search(generation, turn_tools) {
        tools.push(tool_search_spec());
    }
    (tools, deferred.into())
}

pub(crate) fn has_deferred_tools(generation: &Generation, turn_tools: &TurnTools) -> bool {
    generation.tools.entries().iter().any(|entry| {
        turn_tools.permits(&entry.name)
            && turn_tools.effective(&entry.name, entry.visibility) == Visibility::Deferred
    }) || turn_tools.entries().iter().any(|entry| {
        turn_tools.effective(entry.tool.name(), entry.visibility) == Visibility::Deferred
    })
}

pub(crate) fn is_core_tool_search(
    generation: &Generation,
    turn_tools: &TurnTools,
    name: &Name,
) -> bool {
    name.as_str() == TOOL_SEARCH_NAME
        && has_deferred_tools(generation, turn_tools)
        && !has_registered_tool_search(generation, turn_tools)
}

fn has_registered_tool_search(generation: &Generation, turn_tools: &TurnTools) -> bool {
    generation
        .tools
        .entries()
        .iter()
        .any(|entry| entry.name.as_str() == TOOL_SEARCH_NAME && turn_tools.permits(&entry.name))
        || turn_tools
            .entries()
            .iter()
            .any(|entry| entry.tool.name().as_str() == TOOL_SEARCH_NAME)
}

fn deferred_tool_catalog(
    generation: &Generation,
    turn_tools: &TurnTools,
    model: &ModelInfo,
    model_id: &ModelId,
) -> Vec<DeferredTool> {
    let mut deferred = Vec::new();
    for entry in generation.tools.entries() {
        if !turn_tools.permits(&entry.name)
            || turn_tools.effective(&entry.name, entry.visibility) != Visibility::Deferred
        {
            continue;
        }
        if let Some(spec) = generation.tool_spec(&entry.name, model, model_id) {
            deferred.push(DeferredTool {
                name: spec.name.as_str().into(),
                description: spec.description.clone(),
                parameters: spec.parameters.as_str().into(),
            });
        }
    }
    for entry in turn_tools.entries() {
        if turn_tools.effective(entry.tool.name(), entry.visibility) == Visibility::Deferred {
            let spec = entry.tool.spec(model);
            deferred.push(DeferredTool {
                name: spec.name.as_str().into(),
                description: spec.description.clone(),
                parameters: spec.parameters.as_str().into(),
            });
        }
    }
    deferred.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    deferred
}

#[expect(
    clippy::expect_used,
    reason = "tool_search uses a fixed schema literal validated by its request tests"
)]
fn tool_search_spec() -> ModelToolSpec {
    static PARAMETERS: LazyLock<RawJson> = LazyLock::new(|| {
        RawJson::parse(TOOL_SEARCH_PARAMETERS).expect("static tool_search schema")
    });
    ModelToolSpec {
        name: TOOL_SEARCH_NAME.into(),
        description: "Search deferred tools by name or description.".into(),
        parameters: PARAMETERS.clone(),
        grammar: None,
    }
}

pub(crate) fn tool_search_query(args: &RawJson) -> Result<Box<str>, Box<str>> {
    sonic_rs::from_str::<ToolSearchArgs>(args.as_str())
        .map(|args| args.query)
        .map_err(|error| error.to_string().into_boxed_str())
}

pub(crate) fn tool_search_results(query: &str, deferred: &[DeferredTool]) -> Box<str> {
    let query = query.to_lowercase();
    let mut matches: Vec<&DeferredTool> = deferred
        .iter()
        .filter(|tool| {
            tool.name.to_lowercase().contains(&query)
                || tool.description.to_lowercase().contains(&query)
        })
        .collect();
    matches.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    matches.truncate(50);
    if matches.is_empty() {
        return "No deferred tools matched.".into();
    }
    matches
        .into_iter()
        .map(|tool| {
            format!(
                "{}: {}\nparameters: {}",
                tool.name, tool.description, tool.parameters
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        .into()
}

/// Whether a tool visibility reaches the model in this mode.
fn tool_visible(visibility: Option<Visibility>, mode: Mode) -> bool {
    matches!(
        (visibility, mode),
        (Some(Visibility::Model), _)
            | (Some(Visibility::EvalOnly), Mode::EvalFirst | Mode::EvalOnly)
    )
}

/// Byte length of one serialized-equivalent spec for ordering.
fn spec_bytes(spec: &dal_core::ToolSpec) -> usize {
    spec.name.as_str().len() + spec.description.len() + spec.parameters.as_str().len()
}

/// One model-streamed call awaiting resolution.
pub(crate) struct StreamCall {
    /// The provider call identity.
    pub call: CallId,
    /// The named tool.
    pub name: Box<str>,
    /// The raw arguments.
    pub args: RawJson,
}

/// Resolves streamed calls in order, flagging deferred promotions.
pub(crate) fn resolve_calls(
    generation: &Generation,
    turn_tools: &TurnTools,
    calls: &[StreamCall],
    mode: Mode,
    workspace: &Workspace,
) -> Vec<ResolvedCall> {
    let mut seen: HashSet<&CallId> = HashSet::new();
    let mut resolved = Vec::with_capacity(calls.len());
    for call in calls {
        if !seen.insert(&call.call) {
            let Some(name) = sanitized_name(&call.name) else {
                continue;
            };
            resolved.push(ResolvedCall {
                call: call.call.clone(),
                name,
                promoted: false,
                result: Err(ResolveError::DuplicateCallId),
            });
            continue;
        }
        let name = match Name::parse_mapped_tool(&call.name) {
            Ok(name) => name,
            Err(_) => match sanitized_name(&call.name) {
                Some(name) => name,
                None => continue,
            },
        };
        let (promoted, result) =
            resolve_named(generation, turn_tools, &name, &call.args, mode, workspace);
        resolved.push(ResolvedCall {
            call: call.call.clone(),
            name,
            promoted,
            result,
        });
    }
    resolved
}

/// Resolves one named call's arguments: tool lookup, mode gate, and the
/// tool's own classification of those arguments.
///
/// Returns whether the call promotes a deferred tool and its class or the
/// model-visible failure. Dispatch calls this again on the arguments a
/// `tool_call` hook rewrote, so a rewrite meets the same checks the
/// streamed arguments did.
pub(crate) fn resolve_named(
    generation: &Generation,
    turn_tools: &TurnTools,
    name: &Name,
    args: &RawJson,
    mode: Mode,
    workspace: &Workspace,
) -> (bool, Result<ToolClass, ResolveError>) {
    if is_core_tool_search(generation, turn_tools, name) {
        let result = tool_search_query(args)
            .map(|_| ToolClass::Read)
            .map_err(ResolveError::InvalidArgs);
        return (false, result);
    }
    let Some((tool, visibility)) = turn_tools.tool(generation, name) else {
        return (false, Err(ResolveError::Unknown));
    };
    if matches!(visibility, Visibility::EvalOnly)
        && !matches!(mode, Mode::EvalFirst | Mode::EvalOnly)
    {
        return (false, Err(ResolveError::EvalOnly));
    }
    let promoted = matches!(visibility, Visibility::Deferred);
    let result = tool
        .classify(args, workspace)
        .map_err(|error| ResolveError::InvalidArgs(error.to_string().into()));
    (promoted, result)
}

/// Salvages a display name for provider-sent tool names outside the grammar.
///
/// Lowercases ASCII alphanumerics, maps the rest to dashes, and caps at 64
/// bytes; the call still fails `Unknown`, only the message names it. `None`
/// is unreachable (the salvage grammar is closed) and skips the call.
fn sanitized_name(raw: &str) -> Option<Name> {
    let mut text: String = raw
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() {
                byte.to_ascii_lowercase() as char
            } else {
                '-'
            }
        })
        .collect();
    text.truncate(64);
    let trimmed = text.trim_matches('-');
    let fallback = if trimmed.is_empty() {
        "unknown"
    } else {
        trimmed
    };
    Name::parse(fallback).ok()
}

/// Builds provider context messages from leaf entries in order.
///
/// Conversation entries map to their message shape; settings entries carry
/// no conversation content and are skipped. Blob parts stay references;
/// inline images decode from base64, and undecodable bytes skip the part.
/// A journal that holds several results for one tool call yields only the
/// first, because providers reject a second result for the same call.
pub(crate) fn context_items(entries: &[EntryView]) -> Vec<ContextItem> {
    collapse_duplicate_results(compacted_items(entries))
}

/// Maps the leaf entries, replacing the compacted prefix with its summary.
fn compacted_items(entries: &[EntryView]) -> Vec<ContextItem> {
    let Some(compaction_index) = entries
        .iter()
        .rposition(|entry| matches!(&entry.kind, EntryKind::Compaction { .. }))
    else {
        return entries.iter().filter_map(context_item).collect();
    };
    let first_kept = match &entries[compaction_index].kind {
        EntryKind::Compaction { first_kept, .. } => *first_kept,
        _ => None,
    };
    let start = first_kept
        .and_then(|first| {
            entries[..compaction_index]
                .iter()
                .position(|entry| entry.id == first)
        })
        .unwrap_or(compaction_index);
    let mut out = Vec::new();
    if let Some(replacement) = context_item(&entries[compaction_index]) {
        out.push(replacement);
    }
    out.extend(
        entries[start..compaction_index]
            .iter()
            .filter_map(context_item),
    );
    out.extend(
        entries[compaction_index + 1..]
            .iter()
            .filter_map(context_item),
    );
    out
}

/// Keeps the first tool result for each call of one model response.
///
/// Results answer the calls of the assistant message before them, so a call
/// id is tracked from that message until the next one. A later response
/// may reuse an id; its result is kept. The journal is not changed.
fn collapse_duplicate_results(mut items: Vec<ContextItem>) -> Vec<ContextItem> {
    let mut answered: HashSet<&CallId> = HashSet::new();
    let keep: Vec<bool> = items
        .iter()
        .map(|item| match item {
            ContextItem::Assistant { .. } => {
                answered.clear();
                true
            }
            ContextItem::ToolResult { call, .. } => answered.insert(call),
            ContextItem::User { .. } => true,
        })
        .collect();
    let mut keep = keep.into_iter();
    items.retain(|_| keep.next().unwrap_or(true));
    items
}

/// Maps one leaf entry to its provider message, when it carries content.
fn context_item(entry: &EntryView) -> Option<ContextItem> {
    match &entry.kind {
        EntryKind::User { parts } => Some(ContextItem::User {
            parts: parts.iter().filter_map(content_part).collect(),
        }),
        EntryKind::Assistant {
            api,
            model,
            content,
            ..
        } => Some(ContextItem::Assistant {
            source: ReplaySource {
                family: *api,
                model: model.clone(),
            },
            parts: content.iter().map(assistant_part).collect(),
        }),
        EntryKind::ToolResult {
            call,
            name,
            error,
            parts,
            ..
        } => Some(ContextItem::ToolResult {
            call: call.clone(),
            name: name.clone(),
            is_error: *error,
            parts: parts.iter().filter_map(content_part).collect(),
        }),
        EntryKind::Compaction { summary, parts, .. } => {
            let parts: Vec<Part> = if parts.is_empty() {
                summary
                    .as_ref()
                    .map(|text| Part::Text { text: text.clone() })
                    .into_iter()
                    .collect()
            } else {
                parts.iter().filter_map(content_part).collect()
            };
            (!parts.is_empty()).then_some(ContextItem::User { parts })
        }
        _ => None,
    }
}

/// Maps one assistant block to its provider part.
fn assistant_part(block: &Block) -> AssistantPart {
    match block {
        Block::Text { text } => AssistantPart::Text { text: text.clone() },
        Block::Reasoning { text, replay } => AssistantPart::Thinking {
            text: text.clone(),
            replay: Some(replay.clone()),
        },
        Block::ToolCall { id, name, input } => AssistantPart::ToolCall {
            call: id.clone(),
            name: name.clone(),
            args: input.clone(),
        },
    }
}

/// Maps one journal part to inline or referenced content.
fn content_part(part: &JournalPart) -> Option<Part> {
    match part {
        JournalPart::Text { text } => Some(Part::Text { text: text.clone() }),
        JournalPart::TextBlob { blob, bytes } => Some(Part::Blob {
            blob_id: dal_core::BlobId::parse(blob).ok()?,
            mime: "text/plain".into(),
            bytes: *bytes,
        }),
        JournalPart::Image { mime, base64 } => Some(Part::Image {
            mime: mime.clone(),
            bytes: decode_base64(base64)?.into(),
        }),
        JournalPart::ImageBlob { mime, blob, bytes } | JournalPart::Blob { mime, blob, bytes } => {
            Some(Part::Blob {
                blob_id: dal_core::BlobId::parse(blob).ok()?,
                mime: mime.clone(),
                bytes: *bytes,
            })
        }
    }
}

/// Decodes standard base64 bytes for one inline image.
fn decode_base64(raw: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(raw).ok()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use dal_core::{EntryId, EntryKind, JournalPart};

    use super::*;

    fn entry(id: u64, parent: Option<EntryId>, kind: EntryKind) -> EntryView {
        EntryView {
            id: EntryId::new(NonZeroU64::new(id).expect("entry id")),
            parent,
            kind,
        }
    }

    #[test]
    fn compaction_context_replaces_prefix_before_retained_entries() {
        let first = entry(
            1,
            None,
            EntryKind::User {
                parts: vec![JournalPart::Text {
                    text: "removed".into(),
                }],
            },
        );
        let kept = entry(
            2,
            Some(first.id),
            EntryKind::User {
                parts: vec![JournalPart::Text {
                    text: "kept".into(),
                }],
            },
        );
        let compaction = entry(
            3,
            Some(kept.id),
            EntryKind::Compaction {
                summary: Some("replacement".into()),
                first_kept: Some(kept.id),
                tokens_before: 100,
                replay: None,
                usage: None,
                parts: Vec::new(),
                parts_tokens: 0,
            },
        );
        let latest = entry(
            4,
            Some(compaction.id),
            EntryKind::User {
                parts: vec![JournalPart::Text {
                    text: "latest".into(),
                }],
            },
        );

        let context = context_items(&[first, kept, compaction, latest]);

        assert_eq!(
            context,
            vec![
                ContextItem::User {
                    parts: vec![Part::Text {
                        text: "replacement".into()
                    }],
                },
                ContextItem::User {
                    parts: vec![Part::Text {
                        text: "kept".into()
                    }],
                },
                ContextItem::User {
                    parts: vec![Part::Text {
                        text: "latest".into()
                    }],
                },
            ]
        );
    }

    fn assistant_calling(id: u64, parent: Option<EntryId>, calls: &[&str]) -> EntryView {
        entry(
            id,
            parent,
            EntryKind::Assistant {
                api: Family::Chat,
                model: "model".into(),
                content: calls
                    .iter()
                    .map(|call| Block::ToolCall {
                        id: CallId::new(*call),
                        name: "read".into(),
                        input: RawJson::parse("{}").expect("valid JSON"),
                    })
                    .collect(),
                usage: dal_core::Usage {
                    input_tokens: 0,
                    cached_input_tokens: 0,
                    output_tokens: 0,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    cost_usd: None,
                },
                stop: dal_core::AssistantStop::ToolUse,
            },
        )
    }

    fn result_for(id: u64, parent: Option<EntryId>, call: &str, text: &str) -> EntryView {
        entry(
            id,
            parent,
            EntryKind::ToolResult {
                call: CallId::new(call),
                name: "read".into(),
                error: false,
                parts: vec![JournalPart::Text { text: text.into() }],
                changes: Vec::new(),
            },
        )
    }

    fn result_calls(items: &[ContextItem]) -> Vec<(&str, String)> {
        items
            .iter()
            .filter_map(|item| match item {
                ContextItem::ToolResult { call, parts, .. } => Some((
                    call.as_str(),
                    parts
                        .iter()
                        .filter_map(|part| match part {
                            Part::Text { text } => Some(text.as_ref()),
                            _ => None,
                        })
                        .collect(),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn duplicate_tool_results_collapse_to_the_first_per_call() {
        let assistant = assistant_calling(1, None, &["call-a", "call-b"]);
        let first = result_for(2, Some(assistant.id), "call-a", "original");
        let second = result_for(3, Some(first.id), "call-b", "other");
        let duplicate = result_for(4, Some(second.id), "call-a", "replayed");

        let context = context_items(&[assistant, first, second, duplicate]);

        assert_eq!(
            result_calls(&context),
            vec![
                ("call-a", "original".to_owned()),
                ("call-b", "other".to_owned())
            ]
        );
    }

    #[test]
    fn a_call_id_reused_by_a_later_response_keeps_its_own_result() {
        let first_call = assistant_calling(1, None, &["call-0"]);
        let first_result = result_for(2, Some(first_call.id), "call-0", "round one");
        let second_call = assistant_calling(3, Some(first_result.id), &["call-0"]);
        let second_result = result_for(4, Some(second_call.id), "call-0", "round two");

        let context = context_items(&[first_call, first_result, second_call, second_result]);

        assert_eq!(
            result_calls(&context),
            vec![
                ("call-0", "round one".to_owned()),
                ("call-0", "round two".to_owned())
            ]
        );
    }

    #[test]
    fn duplicate_tool_results_collapse_across_a_compaction_boundary() {
        let assistant = assistant_calling(1, None, &["call-a"]);
        let first = result_for(2, Some(assistant.id), "call-a", "original");
        let compaction = entry(
            3,
            Some(first.id),
            EntryKind::Compaction {
                summary: Some("replacement".into()),
                first_kept: Some(assistant.id),
                tokens_before: 100,
                replay: None,
                usage: None,
                parts: Vec::new(),
                parts_tokens: 0,
            },
        );
        let duplicate = result_for(4, Some(compaction.id), "call-a", "replayed");

        let context = context_items(&[assistant, first, compaction, duplicate]);

        assert_eq!(
            result_calls(&context),
            vec![("call-a", "original".to_owned())]
        );
    }

    #[test]
    fn tool_search_matches_name_and_description_case_insensitively_in_byte_order() {
        let deferred = vec![
            DeferredTool {
                name: "zebra".into(),
                description: "Guide for the catalog".into(),
                parameters: r#"{"type":"object"}"#.into(),
            },
            DeferredTool {
                name: "alpha".into(),
                description: "guide".into(),
                parameters: r#"{"type":"object"}"#.into(),
            },
            DeferredTool {
                name: "skip".into(),
                description: "unrelated".into(),
                parameters: r#"{"type":"object"}"#.into(),
            },
        ];
        assert_eq!(
            tool_search_results("GUIDE", &deferred).as_ref(),
            "alpha: guide\nparameters: {\"type\":\"object\"}\nzebra: Guide for the catalog\nparameters: {\"type\":\"object\"}"
        );
    }

    #[test]
    fn tool_search_caps_results_at_fifty() {
        let deferred: Vec<DeferredTool> = (0..51)
            .rev()
            .map(|index| DeferredTool {
                name: format!("tool-{index:02}").into(),
                description: "lookup".into(),
                parameters: r#"{"type":"object"}"#.into(),
            })
            .collect();
        let results = tool_search_results("lookup", &deferred);
        let entries: Vec<&str> = results.lines().collect();
        assert_eq!(entries.len(), 100);
        assert_eq!(entries.first().copied(), Some("tool-00: lookup"));
        assert_eq!(entries.get(98).copied(), Some("tool-49: lookup"));
        assert_eq!(
            entries.last().copied(),
            Some("parameters: {\"type\":\"object\"}")
        );
    }

    #[test]
    fn tool_search_requires_query() {
        let args = RawJson::parse("{}").expect("valid JSON");
        assert!(tool_search_query(&args).is_err());
    }
}
