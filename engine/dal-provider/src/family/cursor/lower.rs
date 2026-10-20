//! Request lowering for the Cursor family: history, tools, and the run frame.
//!
//! [`build_run`] turns one [`ModelRequest`](dal_core::ModelRequest) into the
//! first framed `AgentClientMessage` plus everything the session needs
//! afterwards: the wire route, the advertised tools and rules (for the exec
//! `requestContext` handshake), the blob store seed, and the digest seed the
//! turn capture continues.
//!
//! History lowers to `ConversationStateStructure` blobs: one JSON record per
//! system entry, user message, assistant round, and tool result, following
//! the reference `buildRootPromptMessagesJson` shapes (`role`, `content`
//! with `text` / `reasoning` / `tool-call` / `tool-result` parts). Display
//! turns lower alongside as `AgentConversationTurnStructure` blobs. Tool
//! schemas and argument maps encode as `google.protobuf.Value` bytes. Call
//! ids are normalized to the server charset (`[a-zA-Z0-9_-]`, 64 chars) on
//! both sides of the exchange.
//!
//! Reasoning replays only to the route that wrote it: thinking text lowers
//! as `reasoning` parts solely for assistant entries whose source is this
//! family and model, and server records replay solely when the stored
//! [`CursorHistory`] route and digest still match the current segment.
//!
//! The thinking plan is intentionally not an input: Cursor routes reasoning
//! effort through the model route, so effort arrives inside [`WireModel`]
//! from the catalog, never as a family body fragment.

use std::collections::BTreeMap;

use dal_core::{AssistantPart, ContextItem, Family, ModelRequest, ModelRoute, Part, RawJson};
use prost::Message as _;
use sha2::{Digest as _, Sha256};

use super::proto::{
    AgentClientMessage, AgentConversationTurnStructure, AgentRunRequest, ConversationAction,
    ConversationStateStructure, ConversationStep, ConversationTurnStructure, HistoryToolCall, Json,
    JsonError, McpArgs, McpContentItem, McpImageContent, McpTextContent, McpToolCallHistory,
    McpToolDefinition, McpToolErrorHistory, McpToolResultHistory, McpToolSuccessHistory, McpTools,
    ModelDetails, RequestContext, RequestedModel, RequestedModelParameter, ResumeAction,
    StepAssistantMessage, StepThinkingMessage, UserMessage, UserMessageAction, client_message,
    conversation_action, conversation_step, conversation_turn, history_tool_call, mcp_content_item,
    mcp_tool_result_history, pb_from_json, selected_image, write_json,
};
use crate::tool_names::{WIRE_NAME_MAX, wire_name};

/// The provider identifier this client stamps on advertised tools.
pub(crate) const PROVIDER_IDENTIFIER: &str = "dal-agent";

/// The default system record when the request carries no system prompt.
const DEFAULT_SYSTEM_RECORD: &str = r#"{"role":"system","content":"You are a helpful assistant."}"#;

/// Longest tool-call id the server accepts, in bytes.
const MAX_CALL_ID_LEN: usize = 64;

/// A request that cannot be lowered to a Cursor run frame.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum CursorBodyError {
    /// The request names a non-API model route.
    #[error("a cursor run needs a concrete API model route")]
    NoApiRoute,
    /// A content part still refers to stored content; the caller resolves
    /// blobs to inline bytes before lowering.
    #[error("content blob {blob_id} must be read into the request before a cursor body is built")]
    UnresolvedBlob {
        /// The unresolved blob.
        blob_id: dal_core::BlobId,
    },
    /// A tool schema is not encodable as a wire value.
    #[error("tool {name} has an unencodable schema: {reason}")]
    ToolSchema {
        /// The tool name.
        name: Box<str>,
        /// Why encoding failed.
        reason: JsonError,
    },
    /// Tool-call arguments are not one JSON value.
    #[error("tool call {call} has unencodable arguments: {reason}")]
    ArgsNotJson {
        /// The call id.
        call: Box<str>,
        /// Why encoding failed.
        reason: JsonError,
    },
    /// The run request does not fit the frame length prefix.
    #[error("cursor run request exceeds the frame limit")]
    FrameTooLarge,
}

impl From<CursorBodyError> for crate::error::ProviderError {
    fn from(error: CursorBodyError) -> Self {
        Self::InvalidRequest {
            message: error.to_string(),
        }
    }
}

/// The wire route one request sends: both model identities, the parameter
/// list, and the max-mode flag. The catalog owns effort-to-route mapping;
/// the codec sends whatever route it is given.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WireModel {
    /// The routed model id for `requestedModel.modelId`.
    pub(crate) model_id: Box<str>,
    /// The account-usable slug for `modelDetails.modelId`.
    pub(crate) details_id: Box<str>,
    /// The display name for both detail fields.
    pub(crate) display_name: Box<str>,
    /// Route parameters such as reasoning effort, in order.
    pub(crate) parameters: Vec<(Box<str>, Box<str>)>,
    /// Whether the route needs max mode.
    pub(crate) max_mode: bool,
}

impl WireModel {
    /// A route with no parameters: the model id in every identity field.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn plain(model: &str) -> Self {
        Self {
            model_id: model.into(),
            details_id: model.into(),
            display_name: model.into(),
            parameters: Vec::new(),
            max_mode: false,
        }
    }
}

/// One lowered run: the first request frame plus session seeds.
#[derive(Debug)]
pub(crate) struct LoweredRun {
    /// The framed run-request `AgentClientMessage`, flags unset.
    pub(crate) first_frame: Vec<u8>,
    /// The wire route for digest and replay comparison.
    pub(crate) route: CursorRouteParts,
    /// Advertised tools in registration order (also the exec handshake).
    pub(crate) tools: Vec<McpToolDefinition>,
    /// Request-context rules derived from the system prompt.
    pub(crate) rules: Vec<super::proto::CursorRule>,
    /// Blob id to bytes for every history blob plus the run request parts.
    pub(crate) blobs: BTreeMap<Vec<u8>, Vec<u8>>,
    /// The digest seed: route plus the active user message, if any.
    pub(crate) digest: TurnDigest,
    /// Whether the run opens with a fresh user message.
    pub(crate) has_active_user: bool,
}

/// The route fields a capture binds to, shared by digest and replay.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CursorRouteParts {
    /// The routed model id.
    pub(crate) model: Box<str>,
    /// The details slug.
    pub(crate) details: Box<str>,
    /// Whether max mode was set.
    pub(crate) max_mode: bool,
    /// Route parameters in order.
    pub(crate) parameters: Vec<(Box<str>, Box<str>)>,
}

/// A SHA-256 transcript digest; hex renders the binding token.
#[derive(Clone, Debug)]
pub(crate) struct TurnDigest {
    /// The running hash state.
    state: Sha256,
}

impl TurnDigest {
    /// An empty digest.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            state: Sha256::new(),
        }
    }

    /// Mixes one tagged byte string into the digest.
    fn push(&mut self, tag: &str, bytes: &[u8]) {
        self.state.update(tag.as_bytes());
        self.state.update(b"\0");
        self.state
            .update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        self.state.update(bytes);
    }

    /// Mixes the wire route into the digest.
    pub(crate) fn route(&mut self, route: &CursorRouteParts) {
        self.push("cursor-route/1", route.model.as_bytes());
        self.push("cursor-route/1", route.details.as_bytes());
        self.push("cursor-route/1", if route.max_mode { b"1" } else { b"0" });
        for (id, value) in &route.parameters {
            self.push("cursor-route/1", id.as_bytes());
            self.push("cursor-route/1", value.as_bytes());
        }
    }

    /// Mixes user parts into the digest.
    ///
    /// # Errors
    ///
    /// Returns [`CursorBodyError::UnresolvedBlob`] for a blob part.
    pub(crate) fn user_parts(&mut self, parts: &[Part]) -> Result<(), CursorBodyError> {
        for part in parts {
            match part {
                Part::Text { text } => self.push("cursor-user-text", text.as_bytes()),
                Part::Image { mime, bytes } => {
                    self.push("cursor-user-image", mime.as_bytes());
                    self.push("cursor-user-image", bytes);
                }
                Part::Blob { blob_id, .. } => {
                    return Err(CursorBodyError::UnresolvedBlob { blob_id: *blob_id });
                }
            }
        }
        Ok(())
    }

    /// Mixes assistant parts into the digest: non-empty text, non-empty
    /// thinking, and every tool call with normalized id, internal name,
    /// and canonical arguments.
    pub(crate) fn assistant_parts(&mut self, parts: &[AssistantPart]) {
        for part in parts {
            match part {
                AssistantPart::Text { text, .. } => self.assistant_text(text.as_ref()),
                AssistantPart::Thinking { text, .. } => self.assistant_thinking(text.as_ref()),
                AssistantPart::ToolCall {
                    call, name, args, ..
                } => {
                    self.assistant_tool_call(call.as_str(), name.as_ref(), args.as_str());
                }
            }
        }
    }

    /// Mixes one text run into the digest, skipping empties.
    pub(crate) fn assistant_text(&mut self, text: &str) {
        if !text.is_empty() {
            self.push("cursor-assistant-text", text.as_bytes());
        }
    }

    /// Mixes one thinking run into the digest, skipping empties.
    pub(crate) fn assistant_thinking(&mut self, text: &str) {
        if !text.is_empty() {
            self.push("cursor-assistant-thinking", text.as_bytes());
        }
    }

    /// Mixes one tool call into the digest: the normalized id (both sides
    /// normalize, so wire and journal spellings agree), the internal name,
    /// and the canonical argument text.
    pub(crate) fn assistant_tool_call(&mut self, call: &str, name: &str, args: &str) {
        self.push(
            "cursor-assistant-tool-call",
            normalize_call_id(call).as_bytes(),
        );
        self.push("cursor-assistant-tool-call", name.as_bytes());
        self.push("cursor-assistant-tool-call", args.as_bytes());
    }

    /// Mixes one tool result into the digest.
    pub(crate) fn tool_result(&mut self, call: &str, is_error: bool, parts: &[Part]) {
        self.push("cursor-tool-result", normalize_call_id(call).as_bytes());
        self.push(
            "cursor-tool-result",
            if is_error { b"error" } else { b"ok" },
        );
        for part in parts {
            match part {
                Part::Text { text } => self.push("cursor-tool-result", text.as_bytes()),
                Part::Image { mime, bytes } => {
                    self.push("cursor-tool-result", mime.as_bytes());
                    self.push("cursor-tool-result", bytes);
                }
                Part::Blob { blob_id, .. } => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    let encoded = blob_id
                        .as_bytes()
                        .map(|byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0xF)]]);
                    self.push("cursor-tool-result", encoded.as_flattened());
                }
            }
        }
    }

    /// Renders the digest as lowercase hex.
    #[must_use]
    pub(crate) fn hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let sum = self.state.clone().finalize();
        let mut out = String::with_capacity(sum.len() * 2);
        for byte in sum {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0xF) as usize] as char);
        }
        out
    }
}

/// Encodes the run request carrying the lowered state as the stream's
/// first frame.
fn encode_run_frame(
    root: Vec<Vec<u8>>,
    turns: Vec<Vec<u8>>,
    action: Action,
    rules: &[super::proto::CursorRule],
    tools: &[McpToolDefinition],
    display_model_id: &str,
    wire: &WireModel,
    conversation_id: &str,
) -> Result<Vec<u8>, CursorBodyError> {
    let run_request = AgentRunRequest {
        conversation_state: Some(ConversationStateStructure {
            root_prompt_messages_json: root,
            turns,
        }),
        action: Some(action.into_proto(rules, tools)),
        model_details: Some(ModelDetails {
            model_id: wire.details_id.to_string(),
            display_model_id: display_model_id.to_string(),
            display_name: wire.display_name.to_string(),
            display_name_short: wire.display_name.to_string(),
            aliases: Vec::new(),
            thinking_details: None,
            max_mode: wire.max_mode.then_some(true),
        }),
        requested_model: Some(RequestedModel {
            model_id: wire.model_id.to_string(),
            max_mode: wire.max_mode,
            parameters: wire
                .parameters
                .iter()
                .map(|(id, value)| RequestedModelParameter {
                    id: id.to_string(),
                    value: value.to_string(),
                })
                .collect(),
        }),
        mcp_tools: Some(McpTools {
            tools: tools.to_vec(),
        }),
        conversation_id: Some(conversation_id.to_owned()),
    };
    let client_message = AgentClientMessage {
        message: Some(client_message::Body::RunRequest(run_request)),
    };
    super::frame::encode_frame(&client_message.encode_to_vec(), 0)
        .map_err(|_| CursorBodyError::FrameTooLarge)
}

/// Builds the run request frame and session seeds for one request.
///
/// The trailing user message rides the action; anything else (trailing tool
/// results, a resumed turn) is a resume over the full lowered history.
///
/// # Errors
///
/// Returns [`CursorBodyError`] for a non-API route, an unresolved blob, or
/// an unencodable schema or argument value.
pub(crate) fn build_run(
    request: &ModelRequest,
    wire: &WireModel,
    conversation_id: &str,
    message_id: &str,
) -> Result<LoweredRun, CursorBodyError> {
    let ModelRoute::Api { model, .. } = &request.model else {
        return Err(CursorBodyError::NoApiRoute);
    };
    let route = CursorRouteParts {
        model: model.clone(),
        details: wire.details_id.clone(),
        max_mode: wire.max_mode,
        parameters: wire.parameters.clone(),
    };
    let tools = advertise(&request.tools)?;
    let rules = request_rules(request.system.as_ref());
    let active = match request.context.last() {
        Some(ContextItem::User { parts }) => user_content(parts)?,
        Some(_) | None => None,
    };
    let history_end = if active.is_some() {
        request.context.len() - 1
    } else {
        request.context.len()
    };
    let history = &request.context[..history_end];
    let has_active_user = active.is_some();

    // Allocate unique wire ids for every internal call id in history order
    // before lowering, so root records and display turns agree even when two
    // distinct ids normalize alike. Exact repeats reuse their wire id.
    let mut wire_ids = WireIds::new();
    for item in history {
        if let ContextItem::Assistant { parts, .. } = item {
            for part in parts {
                if let AssistantPart::ToolCall { call, .. } = part {
                    wire_ids.wire(call.as_str());
                }
            }
        }
    }
    let mut store = BlobStore::new();
    let mut root: Vec<Vec<u8>> = Vec::new();
    for record in system_records(request.system.as_ref()) {
        root.push(store.insert(record.as_bytes()));
    }
    let replays = match_replays(history, model.as_ref(), &route)?;
    lower_root_entries(
        history,
        model.as_ref(),
        &replays,
        &wire_ids,
        &mut store,
        &mut root,
    )?;
    let turns = lower_turns(history, model.as_ref(), &wire_ids, &mut store)?;

    let action = match active {
        Some(content) => Action::User {
            text: content.text,
            message_id: message_id.to_owned(),
            images: content.images,
        },
        None => Action::Resume,
    };

    let first_frame = encode_run_frame(
        root,
        turns,
        action,
        &rules,
        &tools,
        model.as_ref(),
        wire,
        conversation_id,
    )?;

    let mut digest = TurnDigest::new();
    digest.route(&route);
    if let Some(ContextItem::User { parts }) = request.context.last().filter(|_| has_active_user) {
        digest.user_parts(parts)?;
    }
    Ok(LoweredRun {
        first_frame,
        route,
        tools,
        rules,
        blobs: store.into_map(),
        digest,
        has_active_user,
    })
}

/// Advertises every dal tool as an MCP definition in registration order.
fn advertise(tools: &[dal_core::ModelToolSpec]) -> Result<Vec<McpToolDefinition>, CursorBodyError> {
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        let schema = super::proto::parse_json(tool.parameters.as_str()).map_err(|reason| {
            CursorBodyError::ToolSchema {
                name: tool.name.clone(),
                reason,
            }
        })?;
        let value = pb_from_json(&schema).map_err(|reason| CursorBodyError::ToolSchema {
            name: tool.name.clone(),
            reason,
        })?;
        let wire = wire_name(tool.name.as_ref(), WIRE_NAME_MAX);
        out.push(McpToolDefinition {
            name: wire.to_string(),
            provider_identifier: String::from(PROVIDER_IDENTIFIER),
            tool_name: wire.to_string(),
            description: tool.description.to_string(),
            input_schema: value.encode_to_vec(),
        });
    }
    Ok(out)
}

/// One system-prompt entry per rule, plus the greeting fallback record.
fn system_records(system: &str) -> Vec<String> {
    if system.trim().is_empty() {
        return vec![String::from(DEFAULT_SYSTEM_RECORD)];
    }
    vec![record_string(&Json::Obj(
        [
            (String::from("role"), Json::Str(String::from("system"))),
            (String::from("content"), Json::Str(system.to_owned())),
        ]
        .into_iter()
        .collect(),
    ))]
}

/// Request-context rules: one global always-apply rule per system prompt.
fn request_rules(system: &str) -> Vec<super::proto::CursorRule> {
    if system.trim().is_empty() {
        return Vec::new();
    }
    vec![super::proto::CursorRule {
        full_path: String::from("/dal/system-prompt/0.mdc"),
        content: system.to_owned(),
        rule_type: Some(super::proto::CursorRuleType {
            rule_type: Some(super::proto::cursor_rule_type::Kind::Global(
                super::proto::CursorRuleTypeGlobal {},
            )),
        }),
        source: super::proto::CursorRuleSource::USER,
    }]
}

/// The turn opener: user message action or resume.
enum Action {
    /// A fresh user message.
    User {
        /// The message text.
        text: String,
        /// The message id, minted per request.
        message_id: String,
        /// Inline images in order.
        images: Vec<(Box<str>, Vec<u8>)>,
    },
    /// History only.
    Resume,
}

impl Action {
    /// Converts the opener into its protobuf action with rules and tools.
    fn into_proto(
        self,
        rules: &[super::proto::CursorRule],
        tools: &[McpToolDefinition],
    ) -> ConversationAction {
        match self {
            Self::User {
                text,
                message_id,
                images,
            } => ConversationAction {
                action: Some(conversation_action::Action::UserMessageAction(
                    UserMessageAction {
                        user_message: Some(UserMessage {
                            text,
                            message_id,
                            selected_context: if images.is_empty() {
                                None
                            } else {
                                Some(super::proto::SelectedContext {
                                    selected_images: images
                                        .into_iter()
                                        .map(|(mime, bytes)| super::proto::SelectedImage {
                                            uuid: uuid_text(),
                                            mime_type: mime.to_string(),
                                            data_or_blob_id: Some(selected_image::Data::Data(
                                                bytes,
                                            )),
                                        })
                                        .collect(),
                                })
                            },
                            mode: 0,
                        }),
                        request_context: Some(RequestContext {
                            rules: rules.to_vec(),
                            env: None,
                            tools: tools.to_vec(),
                            file_contents: BTreeMap::new(),
                            // The opening action declares the same opt-out the
                            // request-context handshake answers with: hosted web
                            // search stays off for the whole run.
                            web_search_enabled: Some(false),
                        }),
                    },
                )),
            },
            Self::Resume => ConversationAction {
                action: Some(conversation_action::Action::ResumeAction(ResumeAction {})),
            },
        }
    }
}

/// The stored server records of one turn, bound to its route and digest.
///
/// The fold journals this as the assistant entry's replay payload; the next
/// request replays `records` verbatim when the route still matches and the
/// recomputed digest equals `digest`, and rebuilds history otherwise.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CursorHistory {
    /// Always `"cursorHistory"`.
    #[serde(rename = "type")]
    pub(crate) kind: String,
    /// Always 1.
    pub(crate) v: u32,
    /// The route that wrote the records.
    pub(crate) route: HistoryRoute,
    /// The turn digest the records stand for.
    pub(crate) digest: String,
    /// Internal call ids whose results the records carry.
    pub(crate) calls: Vec<String>,
    /// The server record texts after the turn's user record, in order.
    pub(crate) records: Vec<String>,
}

/// The route fields a [`CursorHistory`] binds to.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct HistoryRoute {
    /// The routed model id.
    pub(crate) model: String,
    /// The details slug.
    pub(crate) details: String,
    /// Whether max mode was set.
    pub(crate) max_mode: bool,
    /// Route parameters in order.
    pub(crate) parameters: Vec<(String, String)>,
}

/// The history payload marker.
pub(crate) const HISTORY_KIND: &str = "cursorHistory";

/// The history payload version this module reads and writes.
pub(crate) const HISTORY_VERSION: u32 = 1;

impl CursorHistory {
    /// Builds a payload for a finished turn.
    #[must_use]
    pub(crate) fn new(
        route: &CursorRouteParts,
        digest: String,
        calls: Vec<String>,
        records: Vec<String>,
    ) -> Self {
        Self {
            kind: String::from(HISTORY_KIND),
            v: HISTORY_VERSION,
            route: HistoryRoute {
                model: route.model.to_string(),
                details: route.details.to_string(),
                max_mode: route.max_mode,
                parameters: route
                    .parameters
                    .iter()
                    .map(|(id, value)| (id.to_string(), value.to_string()))
                    .collect(),
            },
            digest,
            calls,
            records,
        }
    }

    /// Serializes the payload for journaling; `None` is unreachable for
    /// constructed values and drops the capture instead of failing the turn.
    #[must_use]
    pub(crate) fn to_payload(&self) -> Option<RawJson> {
        let text = sonic_rs::to_string(self).ok()?;
        RawJson::parse(&text).ok()
    }
}

/// Parses a journal replay payload; foreign payloads read as absent.
fn parse_history(raw: &RawJson) -> Option<CursorHistory> {
    let history: CursorHistory = sonic_rs::from_str(raw.as_str()).ok()?;
    (history.kind == HISTORY_KIND && history.v == HISTORY_VERSION).then_some(history)
}

/// Whether a stored payload was written for this route.
fn route_matches(history: &CursorHistory, route: &CursorRouteParts) -> bool {
    history.route.model == route.model.as_ref()
        && history.route.details == route.details.as_ref()
        && history.route.max_mode == route.max_mode
        && history.route.parameters.len() == route.parameters.len()
        && history
            .route
            .parameters
            .iter()
            .zip(route.parameters.iter())
            .all(|((id, value), (rid, rvalue))| id == rid.as_ref() && value == rvalue.as_ref())
}

/// One segment replay: the assistant entry whose records stand for its
/// segment, and the user entry that opens it.
pub(crate) struct SegmentReplay {
    /// The history index of the opening user entry.
    pub(crate) user: usize,
    /// The history index of the captured assistant entry.
    pub(crate) assistant: usize,
    /// The stored records.
    pub(crate) history: CursorHistory,
}

/// Finds every assistant entry whose stored records still stand for its
/// segment: same family and model, same route, and a digest over the
/// opening user entry plus the segment through the assistant that still
/// matches.
///
/// # Errors
///
/// Returns [`CursorBodyError::UnresolvedBlob`] for a blob user part.
pub(crate) fn match_replays(
    history: &[ContextItem],
    model: &str,
    route: &CursorRouteParts,
) -> Result<Vec<SegmentReplay>, CursorBodyError> {
    let mut out = Vec::new();
    let mut user: Option<usize> = None;
    let mut candidate: Option<SegmentReplay> = None;
    for (index, item) in history.iter().enumerate() {
        match item {
            ContextItem::User { .. } => {
                if let Some(replay) = candidate.take() {
                    out.push(replay);
                }
                user = Some(index);
            }
            ContextItem::Assistant { source, replay, .. } => {
                let Some(user_idx) = user else { continue };
                if source.family != Family::Cursor || source.model.as_ref() != model {
                    continue;
                }
                let Some(raw) = replay else { continue };
                let Some(stored) = parse_history(raw) else {
                    continue;
                };
                if !route_matches(&stored, route) {
                    continue;
                }
                let Some(ContextItem::User { parts }) = history.get(user_idx) else {
                    continue;
                };
                let mut digest = TurnDigest::new();
                digest.route(route);
                digest.user_parts(parts)?;
                digest_segment(&mut digest, &history[user_idx + 1..=index]);
                if digest.hex() != stored.digest {
                    continue;
                }
                candidate = Some(SegmentReplay {
                    user: user_idx,
                    assistant: index,
                    history: stored,
                });
            }
            ContextItem::ToolResult { .. } => {}
        }
    }
    if let Some(replay) = candidate.take() {
        out.push(replay);
    }
    Ok(out)
}

/// Mixes one segment slice into the digest in journal order.
fn digest_segment(digest: &mut TurnDigest, items: &[ContextItem]) {
    for item in items {
        match item {
            ContextItem::User { .. } => {}
            ContextItem::Assistant { parts, .. } => digest.assistant_parts(parts),
            ContextItem::ToolResult {
                call,
                is_error,
                parts,
                ..
            } => digest.tool_result(call.as_str(), *is_error, parts),
        }
    }
}

/// Normalizes an internal call id to the server charset.
#[must_use]
pub(crate) fn normalize_call_id(id: &str) -> String {
    let mut out = String::new();
    for character in id.chars() {
        if out.len() >= MAX_CALL_ID_LEN {
            break;
        }
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            out.push(character);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        String::from("call")
    } else {
        out
    }
}

/// Maps internal call ids to unique wire ids.
///
/// Two distinct internal ids can normalize to the same server id (`a.b` and
/// `a_b` both become `a_b`); the server rejects a history that repeats a
/// wire id. The table keeps the first-seen mapping and suffixes later
/// collisions (`a_b`, `a_b_2`, ...) so every valid tool and its result still
/// pairs. Exact repeats of one internal id reuse its wire id, so the
/// existing issued-once dedup still drops them. Digests intentionally keep
/// the normalized base; only emission uses these unique wires.
#[derive(Clone, Debug, Default)]
pub(crate) struct WireIds {
    /// Internal id to assigned wire id, first-seen wins.
    map: std::collections::HashMap<String, String>,
    /// Wire ids already taken, for collision checks.
    used: std::collections::HashSet<String>,
}

impl WireIds {
    /// An empty table.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns the wire id for `internal`, allocating a suffixed variant on
    /// normalized collisions. Exact repeats reuse their first wire id.
    pub(crate) fn wire(&mut self, internal: &str) -> String {
        if let Some(wire) = self.map.get(internal) {
            return wire.clone();
        }
        let base = normalize_call_id(internal);
        if !self.used.contains(&base) {
            self.used.insert(base.clone());
            self.map.insert(internal.to_owned(), base.clone());
            return base;
        }
        let mut counter: u32 = 2;
        loop {
            let suffix = format!("_{counter}");
            let room = MAX_CALL_ID_LEN.saturating_sub(suffix.len());
            let mut stem = base.clone();
            if stem.len() > room {
                stem.truncate(room);
                while !stem.is_char_boundary(stem.len()) {
                    stem.pop();
                }
            }
            if stem.is_empty() {
                stem.push_str("call");
                if stem.len() > room {
                    stem.truncate(room);
                }
            }
            let candidate = format!("{stem}{suffix}");
            if !self.used.contains(&candidate) {
                self.used.insert(candidate.clone());
                self.map.insert(internal.to_owned(), candidate.clone());
                return candidate;
            }
            counter += 1;
        }
    }

    /// Returns the assigned wire id, or the normalized base when `internal`
    /// was never allocated (defensive; callers allocate upfront).
    #[must_use]
    pub(crate) fn lookup(&self, internal: &str) -> String {
        self.map
            .get(internal)
            .cloned()
            .unwrap_or_else(|| normalize_call_id(internal))
    }
}

/// A minted request id.
#[must_use]
pub(crate) fn uuid_text() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A deterministic history user-message id for turn `turn` with `text`.
fn user_message_id(turn: usize, text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hasher = Sha256::new();
    hasher.update(b"cursor-user-message");
    hasher.update([0]);
    hasher.update(turn.to_le_bytes());
    hasher.update(text.as_bytes());
    let sum = hasher.finalize();
    let mut out = String::with_capacity(32);
    for byte in sum.iter().take(16) {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xF) as usize] as char);
    }
    out
}

/// Content-addressed history blobs keyed by raw SHA-256 bytes.
pub(crate) struct BlobStore {
    /// Blob id to bytes.
    map: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl BlobStore {
    /// An empty store.
    pub(crate) fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }

    /// Stores `bytes` under its SHA-256 id, returning the id.
    pub(crate) fn insert(&mut self, bytes: &[u8]) -> Vec<u8> {
        let id = blob_id(bytes);
        self.map.insert(id.clone(), bytes.to_vec());
        id
    }

    /// Consumes the store into its id-to-bytes map.
    #[must_use]
    pub(crate) fn into_map(self) -> BTreeMap<Vec<u8>, Vec<u8>> {
        self.map
    }
}

/// The SHA-256 blob id of `bytes`.
#[must_use]
pub(crate) fn blob_id(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

/// The active user content: joined text plus inline images.
///
/// Returns `None` for an empty message (blank text, no images), which the
/// run sends as a resume instead.
///
/// # Errors
///
/// Returns [`CursorBodyError::UnresolvedBlob`] for a blob part.
fn user_content(parts: &[Part]) -> Result<Option<ActiveContent>, CursorBodyError> {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => texts.push(text.as_ref()),
            Part::Image { mime, bytes } => images.push((mime.clone(), bytes.to_vec())),
            Part::Blob { blob_id, .. } => {
                return Err(CursorBodyError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    let text = texts.join("\n");
    if text.trim().is_empty() && images.is_empty() {
        return Ok(None);
    }
    Ok(Some(ActiveContent { text, images }))
}

/// The lowered active user message.
struct ActiveContent {
    /// The message text.
    text: String,
    /// Inline images in order.
    images: Vec<(Box<str>, Vec<u8>)>,
}

/// Writes one JSON record string.
fn record_string(value: &Json) -> String {
    write_json(value)
}

/// Whether `index` lies strictly inside an accepted replay segment.
///
/// `replays` is sorted by `assistant` ascending with at most one carrier
/// per segment, as `match_replays` produces: the first carrier after
/// `index` covers `index` exactly when its segment opened before `index`.
fn covered_by_replay(replays: &[SegmentReplay], index: usize) -> bool {
    let later = replays.partition_point(|replay| replay.assistant <= index);
    later < replays.len() && replays[later].user < index
}

/// Builds the `rootPromptMessagesJson` entries: system records, then one
/// user record, assistant rounds, and tool results per history item, with
/// stored records substituted for replayed segments.
///
/// # Errors
///
/// Returns [`CursorBodyError`] for blob parts or unencodable arguments.
pub(crate) fn lower_root_entries(
    history: &[ContextItem],
    model: &str,
    replays: &[SegmentReplay],
    wire_ids: &WireIds,
    store: &mut BlobStore,
    root: &mut Vec<Vec<u8>>,
) -> Result<(), CursorBodyError> {
    let replay_at: BTreeMap<usize, &CursorHistory> = replays
        .iter()
        .map(|replay| (replay.assistant, &replay.history))
        .collect();
    let mut paired = std::collections::HashSet::new();
    for item in history {
        if let ContextItem::Assistant { parts, .. } = item {
            for part in parts {
                if let AssistantPart::ToolCall { call, .. } = part {
                    paired.insert(call.as_str());
                }
            }
        }
    }
    let mut last_result: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (index, item) in history.iter().enumerate() {
        if let ContextItem::ToolResult { call, .. } = item {
            last_result.insert(call.as_str(), index);
        }
    }
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (index, item) in history.iter().enumerate() {
        if covered_by_replay(replays, index) {
            continue;
        }
        match item {
            ContextItem::User { parts } => {
                if let Some(record) = user_record(parts)? {
                    root.push(store.insert(record.as_bytes()));
                }
            }
            ContextItem::Assistant { source, parts, .. } => {
                if let Some(stored) = replay_at.get(&index) {
                    // Stored calls are already wire spellings: the capture
                    // normalizes before it stores, so they must not pass
                    // through the internal-to-wire table again. A table
                    // lookup of a stored wire would hit a colliding internal
                    // of the same spelling and re-emit the covered result.
                    emitted.extend(stored.calls.iter().map(|call| normalize_call_id(call)));
                    for record in &stored.records {
                        root.push(store.insert(record.as_bytes()));
                    }
                    continue;
                }
                let rounds = if source.family == Family::Cursor && source.model.as_ref() == model {
                    split_rounds(parts, wire_ids)
                } else {
                    split_rounds_without_thinking(parts, wire_ids)
                };
                let last_round = rounds.len().saturating_sub(1);
                for (round_index, round) in rounds.iter().enumerate() {
                    root.push(store.insert(assistant_record(&round.content).as_bytes()));
                    if round_index == last_round {
                        continue;
                    }
                    emit_round_results(
                        history,
                        index,
                        &round.calls,
                        &paired,
                        &last_result,
                        replays,
                        wire_ids,
                        &mut emitted,
                        store,
                        root,
                    )?;
                }
            }
            ContextItem::ToolResult {
                call,
                name,
                is_error,
                parts,
            } => {
                let wire = wire_ids.lookup(call.as_str());
                if emitted.contains(&wire) {
                    continue;
                }
                if !paired.contains(call.as_str()) {
                    root.push(store.insert(orphan_record(*is_error, parts)?.as_bytes()));
                    continue;
                }
                if last_result.get(call.as_str()) != Some(&index) {
                    continue;
                }
                emitted.insert(wire);
                root.push(
                    store.insert(
                        tool_result_record(
                            call.as_str(),
                            name.as_ref(),
                            *is_error,
                            parts,
                            wire_ids,
                        )?
                        .as_bytes(),
                    ),
                );
            }
        }
    }
    Ok(())
}

/// One assistant round: content plus the call ids it issued.
struct Round<'a> {
    /// The record content parts.
    content: Vec<Json>,
    /// Internal call ids issued in order.
    calls: Vec<&'a str>,
}

/// Splits assistant parts into model rounds with thinking intact: a round
/// ends at a call followed by anything but another call; consecutive calls
/// stay together. For entries this family and model wrote.
fn split_rounds<'a>(parts: &'a [AssistantPart], wire_ids: &WireIds) -> Vec<Round<'a>> {
    split_rounds_impl(parts, true, wire_ids)
}

/// Splits assistant parts into model rounds without thinking: foreign
/// reasoning replays solely through server records, never rebuilt here.
fn split_rounds_without_thinking<'a>(
    parts: &'a [AssistantPart],
    wire_ids: &WireIds,
) -> Vec<Round<'a>> {
    split_rounds_impl(parts, false, wire_ids)
}

/// The shared round splitter; `thinking` selects the thinking policy its
/// two named entry points pin down.
fn split_rounds_impl<'a>(
    parts: &'a [AssistantPart],
    thinking: bool,
    wire_ids: &WireIds,
) -> Vec<Round<'a>> {
    let mut rounds = Vec::new();
    let mut current = Round {
        content: Vec::new(),
        calls: Vec::new(),
    };
    let mut issued: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for part in parts {
        // A call id repeated within one entry, a redispatched exec frame
        // recorded before redispatch was deduplicated, is issued once: the
        // server rejects a history that repeats it. The display turns apply
        // the same rule. The dropped repeat neither closes a round nor adds
        // content, and its results settle through the last-result rule.
        if let AssistantPart::ToolCall { call, .. } = part
            && !issued.insert(call.as_str())
        {
            continue;
        }
        let is_call = matches!(part, AssistantPart::ToolCall { .. });
        if !is_call && !current.calls.is_empty() {
            rounds.push(current);
            current = Round {
                content: Vec::new(),
                calls: Vec::new(),
            };
        }
        match part {
            AssistantPart::Text { text, .. } => {
                if !text.is_empty() {
                    current.content.push(text_part(text.as_ref()));
                }
            }
            AssistantPart::Thinking { text, .. } => {
                if thinking && !text.is_empty() {
                    current.content.push(reasoning_part(text.as_ref()));
                }
            }
            AssistantPart::ToolCall {
                call, name, args, ..
            } => {
                current.calls.push(call.as_str());
                current
                    .content
                    .push(tool_call_part(call.as_str(), name.as_ref(), args, wire_ids));
            }
        }
    }
    if !current.content.is_empty() {
        rounds.push(current);
    }
    rounds
}

/// Emits the results of a non-final round in arrival order: the first
/// position triggers, the last occurrence supplies the content.
fn emit_round_results(
    history: &[ContextItem],
    assistant: usize,
    calls: &[&str],
    paired: &std::collections::HashSet<&str>,
    last_result: &std::collections::HashMap<&str, usize>,
    replays: &[SegmentReplay],
    wire_ids: &WireIds,
    emitted: &mut std::collections::HashSet<String>,
    store: &mut BlobStore,
    root: &mut Vec<Vec<u8>>,
) -> Result<(), CursorBodyError> {
    let mut order: Vec<&str> = Vec::new();
    for later in &history[assistant + 1..] {
        match later {
            ContextItem::User { .. } | ContextItem::Assistant { .. } => {}
            ContextItem::ToolResult { call, .. } => {
                let id = call.as_str();
                if calls.contains(&id) && paired.contains(id) && !order.contains(&id) {
                    order.push(id);
                }
            }
        }
    }
    for id in order {
        let wire = wire_ids.lookup(id);
        if emitted.contains(&wire) {
            continue;
        }
        let Some(&at) = last_result.get(id) else {
            continue;
        };
        if covered_by_replay(replays, at) {
            continue;
        }
        let Some(ContextItem::ToolResult {
            name,
            is_error,
            parts,
            ..
        }) = history.get(at)
        else {
            continue;
        };
        emitted.insert(wire);
        root.push(
            store.insert(
                tool_result_record(id, name.as_ref(), *is_error, parts, wire_ids)?.as_bytes(),
            ),
        );
    }
    Ok(())
}

/// Builds the display turns: one user blob plus step blobs per turn.
///
/// Thinking lowers only for entries this family and model wrote; every
/// other entry's reasoning replays solely through server records.
///
/// # Errors
///
/// Returns [`CursorBodyError`] for blob parts or unencodable arguments.
#[expect(
    clippy::too_many_lines,
    reason = "one linear pass walks history items, user messages, assistant replies, and tool results"
)]
pub(crate) fn lower_turns(
    history: &[ContextItem],
    model: &str,
    wire_ids: &WireIds,
    store: &mut BlobStore,
) -> Result<Vec<Vec<u8>>, CursorBodyError> {
    let mut turns = Vec::new();
    let mut index = 0;
    let mut turn_number = 0_usize;
    while index < history.len() {
        let Some(ContextItem::User { parts }) = history.get(index) else {
            index += 1;
            continue;
        };
        let Some(content) = user_content(parts)? else {
            index += 1;
            continue;
        };
        let user = UserMessage {
            text: content.text.clone(),
            message_id: user_message_id(turn_number, &content.text),
            selected_context: if content.images.is_empty() {
                None
            } else {
                Some(super::proto::SelectedContext {
                    selected_images: content
                        .images
                        .iter()
                        .map(|(mime, bytes)| super::proto::SelectedImage {
                            uuid: uuid_text(),
                            mime_type: mime.to_string(),
                            data_or_blob_id: Some(selected_image::Data::Data(bytes.clone())),
                        })
                        .collect(),
                })
            },
            mode: 0,
        };
        let user_blob = store.insert(&user.encode_to_vec());
        let mut steps: Vec<Vec<u8>> = Vec::new();
        index += 1;
        while index < history.len() {
            match &history[index] {
                ContextItem::User { .. } => break,
                ContextItem::Assistant { source, parts, .. } => {
                    let replayable =
                        source.family == Family::Cursor && source.model.as_ref() == model;
                    let mut issued: std::collections::HashSet<&str> =
                        std::collections::HashSet::new();
                    for part in parts {
                        match part {
                            AssistantPart::Text { text, .. } => {
                                if text.is_empty() {
                                    continue;
                                }
                                steps.push(
                                    store.insert(
                                        &ConversationStep {
                                            message: Some(
                                                conversation_step::Step::AssistantMessage(
                                                    StepAssistantMessage {
                                                        text: text.to_string(),
                                                    },
                                                ),
                                            ),
                                        }
                                        .encode_to_vec(),
                                    ),
                                );
                            }
                            AssistantPart::Thinking { text, .. } => {
                                if text.is_empty() || !replayable {
                                    continue;
                                }
                                steps.push(
                                    store.insert(
                                        &ConversationStep {
                                            message: Some(
                                                conversation_step::Step::ThinkingMessage(
                                                    StepThinkingMessage {
                                                        text: text.to_string(),
                                                    },
                                                ),
                                            ),
                                        }
                                        .encode_to_vec(),
                                    ),
                                );
                            }
                            AssistantPart::ToolCall {
                                call, name, args, ..
                            } => {
                                if !issued.insert(call.as_str()) {
                                    continue;
                                }
                                let result = history_result(history, index, call.as_str())?;
                                steps.push(
                                    store.insert(
                                        &ConversationStep {
                                            message: Some(conversation_step::Step::ToolCall(
                                                HistoryToolCall {
                                                    tool: Some(
                                                        history_tool_call::Tool::McpToolCall(
                                                            McpToolCallHistory {
                                                                args: Some(history_args(
                                                                    call.as_str(),
                                                                    name.as_ref(),
                                                                    args,
                                                                    wire_ids,
                                                                )?),
                                                                result,
                                                            },
                                                        ),
                                                    ),
                                                    tool_call_id: Some(
                                                        wire_ids.lookup(call.as_str()),
                                                    ),
                                                },
                                            )),
                                        }
                                        .encode_to_vec(),
                                    ),
                                );
                            }
                        }
                    }
                }
                ContextItem::ToolResult {
                    call,
                    is_error,
                    parts,
                    ..
                } => {
                    if paired_call(history, index, call.as_str()) {
                        index += 1;
                        continue;
                    }
                    let text = orphan_record(*is_error, parts)?;
                    steps.push(
                        store.insert(
                            &ConversationStep {
                                message: Some(conversation_step::Step::AssistantMessage(
                                    StepAssistantMessage { text },
                                )),
                            }
                            .encode_to_vec(),
                        ),
                    );
                }
            }
            index += 1;
        }
        let turn = ConversationTurnStructure {
            turn: Some(conversation_turn::Turn::AgentConversationTurn(
                AgentConversationTurnStructure {
                    user_message: user_blob,
                    steps,
                },
            )),
        };
        turns.push(store.insert(&turn.encode_to_vec()));
        turn_number += 1;
    }
    Ok(turns)
}

/// Whether the tool result at `at` answers a call issued before it.
fn paired_call(history: &[ContextItem], at: usize, call: &str) -> bool {
    history[..at].iter().any(|item| match item {
        ContextItem::Assistant { parts, .. } => parts.iter().any(|part| match part {
            AssistantPart::ToolCall { call: other, .. } => other.as_str() == call,
            AssistantPart::Text { .. } | AssistantPart::Thinking { .. } => false,
        }),
        ContextItem::User { .. } | ContextItem::ToolResult { .. } => false,
    })
}

/// The recorded result for one history call: the last result after its
/// assistant entry, or `None` when unsettled.
fn history_result(
    history: &[ContextItem],
    assistant: usize,
    call: &str,
) -> Result<Option<McpToolResultHistory>, CursorBodyError> {
    let mut found: Option<(&str, bool, &[Part])> = None;
    for later in &history[assistant + 1..] {
        match later {
            ContextItem::User { .. } | ContextItem::Assistant { .. } => {}
            ContextItem::ToolResult {
                call: other,
                name,
                is_error,
                parts,
            } => {
                if other.as_str() == call {
                    found = Some((name.as_ref(), *is_error, parts));
                }
            }
        }
    }
    let Some((_, is_error, parts)) = found else {
        return Ok(None);
    };
    Ok(Some(result_history(is_error, parts)?))
}

/// Converts result parts into a recorded MCP result.
fn result_history(is_error: bool, parts: &[Part]) -> Result<McpToolResultHistory, CursorBodyError> {
    if is_error {
        return Ok(McpToolResultHistory {
            result: Some(mcp_tool_result_history::Result::Error(
                McpToolErrorHistory {
                    error: result_text(parts)?,
                },
            )),
        });
    }
    let mut content = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => content.push(McpContentItem {
                content: Some(mcp_content_item::Content::Text(McpTextContent {
                    text: text.to_string(),
                })),
            }),
            Part::Image { mime, bytes } => content.push(McpContentItem {
                content: Some(mcp_content_item::Content::Image(McpImageContent {
                    data: bytes.to_vec(),
                    mime_type: mime.to_string(),
                })),
            }),
            Part::Blob { blob_id, .. } => {
                return Err(CursorBodyError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    Ok(McpToolResultHistory {
        result: Some(mcp_tool_result_history::Result::Success(
            McpToolSuccessHistory {
                content,
                is_error: false,
            },
        )),
    })
}

/// The MCP arguments for one history call: the canonical object, or an
/// empty map when the recorded value is not an object.
fn history_args(
    call: &str,
    name: &str,
    args: &RawJson,
    wire_ids: &WireIds,
) -> Result<McpArgs, CursorBodyError> {
    let value =
        super::proto::parse_json(args.as_str()).map_err(|reason| CursorBodyError::ArgsNotJson {
            call: call.into(),
            reason,
        })?;
    let fields = match &value {
        Json::Obj(fields) => fields,
        Json::Null
        | Json::Bool(_)
        | Json::Int(_)
        | Json::Uint(_)
        | Json::Float(_)
        | Json::Str(_)
        | Json::Arr(_) => &std::collections::BTreeMap::new(),
    };
    let wire = wire_name(name, WIRE_NAME_MAX);
    let mut encoded = BTreeMap::new();
    for (key, item) in fields {
        let value = pb_from_json(item).map_err(|reason| CursorBodyError::ArgsNotJson {
            call: call.into(),
            reason,
        })?;
        encoded.insert(key.clone(), value.encode_to_vec());
    }
    Ok(McpArgs {
        name: wire.to_string(),
        args: encoded,
        tool_call_id: wire_ids.lookup(call),
        tool_name: wire.to_string(),
        provider_identifier: String::from(PROVIDER_IDENTIFIER),
        smart_mode_approval_only: false,
        skip_approval: false,
        server_identifier: String::new(),
    })
}

/// One user history record, or `None` when empty.
fn user_record(parts: &[Part]) -> Result<Option<String>, CursorBodyError> {
    let mut content = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => {
                if !text.trim().is_empty() {
                    content.push(Json::Obj(
                        [
                            (String::from("type"), Json::Str(String::from("text"))),
                            (String::from("text"), Json::Str(text.to_string())),
                        ]
                        .into_iter()
                        .collect(),
                    ));
                }
            }
            Part::Image { mime, bytes } => {
                content.push(Json::Obj(
                    [
                        (String::from("type"), Json::Str(String::from("image"))),
                        (
                            String::from("image"),
                            Json::Str(format!(
                                "data:{};base64,{}",
                                mime,
                                base64::Engine::encode(
                                    &base64::engine::general_purpose::STANDARD,
                                    bytes
                                )
                            )),
                        ),
                        (String::from("mediaType"), Json::Str(mime.to_string())),
                    ]
                    .into_iter()
                    .collect(),
                ));
            }
            Part::Blob { blob_id, .. } => {
                return Err(CursorBodyError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    if content.is_empty() {
        return Ok(None);
    }
    Ok(Some(record_string(&Json::Obj(
        [
            (String::from("role"), Json::Str(String::from("user"))),
            (String::from("content"), Json::Arr(content)),
        ]
        .into_iter()
        .collect(),
    ))))
}

/// One assistant history record for a round.
fn assistant_record(content: &[Json]) -> String {
    record_string(&Json::Obj(
        [
            (String::from("role"), Json::Str(String::from("assistant"))),
            (String::from("content"), Json::Arr(content.to_vec())),
        ]
        .into_iter()
        .collect(),
    ))
}

/// One reasoning record part.
fn reasoning_part(text: &str) -> Json {
    Json::Obj(
        [
            (String::from("type"), Json::Str(String::from("reasoning"))),
            (String::from("text"), Json::Str(text.to_owned())),
        ]
        .into_iter()
        .collect(),
    )
}

/// One assistant text record part.
fn text_part(text: &str) -> Json {
    Json::Obj(
        [
            (String::from("type"), Json::Str(String::from("text"))),
            (String::from("text"), Json::Str(text.to_owned())),
        ]
        .into_iter()
        .collect(),
    )
}

/// One tool-call record part with canonical arguments.
fn tool_call_part(call: &str, name: &str, args: &RawJson, wire_ids: &WireIds) -> Json {
    let value = super::proto::parse_json(args.as_str()).map_or_else(
        |_| Json::Obj(BTreeMap::new()),
        |parsed| match parsed {
            Json::Obj(_) => parsed,
            Json::Null
            | Json::Bool(_)
            | Json::Int(_)
            | Json::Uint(_)
            | Json::Float(_)
            | Json::Str(_)
            | Json::Arr(_) => Json::Obj(BTreeMap::new()),
        },
    );
    Json::Obj(
        [
            (String::from("type"), Json::Str(String::from("tool-call"))),
            (String::from("toolCallId"), Json::Str(wire_ids.lookup(call))),
            (
                String::from("toolName"),
                Json::Str(wire_name(name, WIRE_NAME_MAX).to_string()),
            ),
            (String::from("args"), value),
        ]
        .into_iter()
        .collect(),
    )
}

/// One tool-result history record.
fn tool_result_record(
    call: &str,
    name: &str,
    is_error: bool,
    parts: &[Part],
    wire_ids: &WireIds,
) -> Result<String, CursorBodyError> {
    // The reference sends the text as it is, even when empty (the paired call
    // is already in history), and marks only a failed result.
    let mut part: BTreeMap<String, Json> = [
        (String::from("type"), Json::Str(String::from("tool-result"))),
        (
            String::from("toolName"),
            Json::Str(wire_name(name, WIRE_NAME_MAX).to_string()),
        ),
        (String::from("toolCallId"), Json::Str(wire_ids.lookup(call))),
        (String::from("result"), Json::Str(result_text(parts)?)),
    ]
    .into_iter()
    .collect();
    if is_error {
        part.insert(String::from("isError"), Json::Bool(true));
    }
    Ok(record_string(&Json::Obj(
        [
            (String::from("role"), Json::Str(String::from("tool"))),
            (String::from("id"), Json::Str(wire_ids.lookup(call))),
            (String::from("content"), Json::Arr(vec![Json::Obj(part)])),
        ]
        .into_iter()
        .collect(),
    )))
}

/// The display text of result parts: text joined by newline, images noted
/// as `[<media type> image]` (their bytes ride the turn steps, not the text
/// record). An empty result stays empty here; only an orphan names itself.
fn result_text(parts: &[Part]) -> Result<String, CursorBodyError> {
    let mut texts = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => texts.push(text.to_string()),
            Part::Image { mime, .. } => texts.push(format!("[{mime} image]")),
            Part::Blob { blob_id, .. } => {
                return Err(CursorBodyError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    Ok(texts.join("\n"))
}

/// An orphan tool result as assistant text, mirroring the reference prefix
/// and its empty-result fallback.
fn orphan_record(is_error: bool, parts: &[Part]) -> Result<String, CursorBodyError> {
    let prefix = if is_error {
        "[Tool Error]"
    } else {
        "[Tool Result]"
    };
    let text = result_text(parts)?;
    let text = if text.is_empty() {
        String::from("(empty result)")
    } else {
        text
    };
    Ok(record_string(&Json::Obj(
        [
            (String::from("role"), Json::Str(String::from("assistant"))),
            (
                String::from("content"),
                Json::Arr(vec![text_part(&format!("{prefix}\n{text}"))]),
            ),
        ]
        .into_iter()
        .collect(),
    )))
}
