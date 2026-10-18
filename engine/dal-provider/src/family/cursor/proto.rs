//! Cursor `agent.v1` protobuf messages as `prost` 0.14.4 derive structs.
//!
//! Field numbers are verbatim from the pinned oh-my-pi `a4dc3302`
//! `agent.proto` / `cursor-proto.ts`. Only the subset the codec sends or
//! reads is modeled: unmodeled oneof arms and message fields decode into
//! nothing (`prost` skips unknown tags), so newer servers stay
//! wire-compatible. Nothing here touches the network; see [`super::frame`]
//! for the envelope and [`super::decode`] for the stream state machine.
//!
//! [`Json`] is the canonical JSON tree shared by schema lowering, argument
//! assembly, and the turn digest: objects keep sorted keys (`BTreeMap`) and
//! integers survive as integers, so two builds of one value are byte for
//! byte equal. [`PbValue`] is the `google.protobuf.Value` layout Cursor
//! uses for tool schemas and per-argument values.

use std::collections::BTreeMap;

use sonic_rs::{JsonContainerTrait, JsonValueTrait};

/// A canonical JSON value: objects sort their keys, integers stay integers.
///
/// Values are built from [`sonic_rs::Value`] with [`parse_json`], never
/// through an untagged `serde` enum, so parsing never depends on content
/// buffering.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    /// JSON `null`.
    Null,
    /// JSON `true` or `false`.
    Bool(bool),
    /// A JSON integer that fits `i64`.
    Int(i64),
    /// A JSON integer above `i64::MAX`.
    Uint(u64),
    /// A JSON number with a fraction or exponent.
    Float(f64),
    /// A JSON string.
    Str(String),
    /// A JSON array.
    Arr(Vec<Json>),
    /// A JSON object with sorted keys.
    Obj(BTreeMap<String, Json>),
}

/// A JSON bridge failure: malformed text, excessive nesting, or a
/// kind-less wire value.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum JsonError {
    /// The text is not one JSON value.
    #[error("invalid JSON: {message}")]
    Parse {
        /// The parser message.
        message: Box<str>,
    },
    /// The value nests too deep for the wire or the converter.
    #[error("JSON value nests too deep for the Cursor wire")]
    TooDeep,
    /// A wire value carries no kind.
    #[error("Cursor wire value carries no kind")]
    NoKind,
}

/// The deepest nesting the JSON bridge traverses before refusing with
/// [`JsonError::TooDeep`]; deeper values are a protocol violation, never a
/// stack overflow.
const MAX_JSON_DEPTH: usize = 64;

/// Parses `text` as exactly one JSON value.
///
/// # Errors
///
/// Returns [`JsonError::Parse`] when `text` is not one JSON value, or
/// [`JsonError::TooDeep`] when it nests past [`MAX_JSON_DEPTH`].
pub(crate) fn parse_json(text: &str) -> Result<Json, JsonError> {
    let value: sonic_rs::Value =
        sonic_rs::from_str(text).map_err(|error: sonic_rs::Error| JsonError::Parse {
            message: error.to_string().into_boxed_str(),
        })?;
    from_value(&value, MAX_JSON_DEPTH)
}

/// Converts a parsed [`sonic_rs::Value`] into canonical [`Json`].
///
/// # Errors
///
/// Returns [`JsonError::TooDeep`] past [`MAX_JSON_DEPTH`].
fn from_value(value: &sonic_rs::Value, depth: usize) -> Result<Json, JsonError> {
    if depth == 0 {
        return Err(JsonError::TooDeep);
    }
    if value.is_null() {
        return Ok(Json::Null);
    }
    if let Some(flag) = value.as_bool() {
        return Ok(Json::Bool(flag));
    }
    if let Some(number) = value.as_i64() {
        return Ok(Json::Int(number));
    }
    if let Some(number) = value.as_u64() {
        return Ok(Json::Uint(number));
    }
    if let Some(number) = value.as_f64() {
        return Ok(Json::Float(number));
    }
    if let Some(text) = value.as_str() {
        return Ok(Json::Str(text.to_owned()));
    }
    if let Some(items) = value.as_array() {
        let mut converted = Vec::with_capacity(items.len());
        for item in items {
            converted.push(from_value(item, depth - 1)?);
        }
        return Ok(Json::Arr(converted));
    }
    if let Some(object) = value.as_object() {
        let mut fields = BTreeMap::new();
        for (key, item) in object {
            fields.insert(key.to_owned(), from_value(item, depth - 1)?);
        }
        return Ok(Json::Obj(fields));
    }
    Err(JsonError::Parse {
        message: String::from("unsupported JSON value").into_boxed_str(),
    })
}

/// Writes `value` in canonical form: sorted object keys, integers without a
/// fraction, no whitespace. Integers that fit `i64` render exactly;
/// `u64` values above `i64::MAX` render exactly; finite floats render in
/// Rust's shortest round-trip form; a non-finite float (unreachable from
/// parsed JSON) renders as `null`, as the reference normalizer does.
#[must_use]
pub(crate) fn write_json(value: &Json) -> String {
    let mut out = String::new();
    push_json(&mut out, value);
    out
}

/// Appends the canonical form of `value` to `out`.
fn push_json(out: &mut String, value: &Json) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(number) => out.push_str(&number.to_string()),
        Json::Uint(number) => out.push_str(&number.to_string()),
        Json::Float(number) => {
            if number.is_finite() {
                out.push_str(&number.to_string());
            } else {
                out.push_str("null");
            }
        }
        Json::Str(text) => push_escaped(out, text),
        Json::Arr(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_json(out, item);
            }
            out.push(']');
        }
        Json::Obj(fields) => {
            out.push('{');
            for (index, (key, item)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_escaped(out, key);
                out.push(':');
                push_json(out, item);
            }
            out.push('}');
        }
    }
}

/// Appends `text` as a double-quoted JSON string with the required escapes.
fn push_escaped(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            other if (other as u32) < 0x20 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let code = other as u32;
                out.push_str("\\u00");
                out.push(HEX[(code >> 4) as usize] as char);
                out.push(HEX[(code & 0xF) as usize] as char);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// Encodes `value` as a `google.protobuf.Value` message.
///
/// Integers encode as doubles because the wire has no integer kind; values
/// past 2^53 lose precision on the wire, which the format forces on every
/// implementation. Non-finite floats encode as `null`, as the reference
/// normalizer does.
///
/// # Errors
///
/// Returns [`JsonError::TooDeep`] when `value` nests past
/// [`MAX_JSON_DEPTH`].
pub(crate) fn pb_from_json(value: &Json) -> Result<PbValue, JsonError> {
    pb_from_json_depth(value, MAX_JSON_DEPTH)
}

/// Encodes `value` with `depth` nesting steps remaining.
///
/// # Errors
///
/// Returns [`JsonError::TooDeep`] when `depth` runs out.
fn pb_from_json_depth(value: &Json, depth: usize) -> Result<PbValue, JsonError> {
    if depth == 0 {
        return Err(JsonError::TooDeep);
    }
    let kind = match value {
        Json::Null => pb_value::Kind::NullValue(0),
        Json::Bool(flag) => pb_value::Kind::BoolValue(*flag),
        // The wire's only number is a double; see `pb_from_json`.
        #[expect(
            clippy::cast_precision_loss,
            reason = "protobuf Value represents all numbers as double floats"
        )]
        Json::Int(number) => pb_value::Kind::NumberValue(*number as f64),
        #[expect(
            clippy::cast_precision_loss,
            reason = "protobuf Value represents all numbers as double floats"
        )]
        Json::Uint(number) => pb_value::Kind::NumberValue(*number as f64),
        Json::Float(number) => {
            if number.is_finite() {
                pb_value::Kind::NumberValue(*number)
            } else {
                pb_value::Kind::NullValue(0)
            }
        }
        Json::Str(text) => pb_value::Kind::StringValue(text.clone()),
        Json::Arr(items) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(pb_from_json_depth(item, depth - 1)?);
            }
            pb_value::Kind::ListValue(PbList { values })
        }
        Json::Obj(fields) => {
            let mut map = BTreeMap::new();
            for (key, item) in fields {
                map.insert(key.clone(), pb_from_json_depth(item, depth - 1)?);
            }
            pb_value::Kind::StructValue(PbStruct { fields: map })
        }
    };
    Ok(PbValue { kind: Some(kind) })
}

/// Decodes a `google.protobuf.Value` message into canonical JSON.
///
/// # Errors
///
/// Returns [`JsonError::TooDeep`] past [`MAX_JSON_DEPTH`], or
/// [`JsonError::NoKind`] when the value carries no kind.
pub(crate) fn json_from_pb(value: &PbValue) -> Result<Json, JsonError> {
    json_from_pb_depth(value, MAX_JSON_DEPTH)
}

/// Decodes `value` with `depth` nesting steps remaining.
///
/// # Errors
///
/// Returns [`JsonError::TooDeep`] when `depth` runs out, or
/// [`JsonError::NoKind`] for a kind-less value.
fn json_from_pb_depth(value: &PbValue, depth: usize) -> Result<Json, JsonError> {
    if depth == 0 {
        return Err(JsonError::TooDeep);
    }
    let Some(kind) = &value.kind else {
        return Err(JsonError::NoKind);
    };
    match kind {
        pb_value::Kind::NullValue(_) => Ok(Json::Null),
        pb_value::Kind::NumberValue(number) => Ok(Json::Float(*number)),
        pb_value::Kind::StringValue(text) => Ok(Json::Str(text.clone())),
        pb_value::Kind::BoolValue(flag) => Ok(Json::Bool(*flag)),
        pb_value::Kind::ListValue(list) => {
            let mut items = Vec::with_capacity(list.values.len());
            for item in &list.values {
                items.push(json_from_pb_depth(item, depth - 1)?);
            }
            Ok(Json::Arr(items))
        }
        pb_value::Kind::StructValue(object) => {
            let mut fields = BTreeMap::new();
            for (key, item) in &object.fields {
                fields.insert(key.clone(), json_from_pb_depth(item, depth - 1)?);
            }
            Ok(Json::Obj(fields))
        }
    }
}

/// A `google.protobuf.Value`, field-for-field: null 1, number 2, string 3,
/// boolean 4, struct 5, list 6.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct PbValue {
    /// The value kind; absent is a protocol violation.
    #[prost(oneof = "pb_value::Kind", tags = "1, 2, 3, 4, 5, 6")]
    pub(crate) kind: Option<pb_value::Kind>,
}

/// Nested kinds of [`PbValue`].
pub(crate) mod pb_value {
    /// One `google.protobuf.Value` kind.
    #[derive(Clone, PartialEq, prost::Oneof)]
    #[expect(
        clippy::enum_variant_names,
        reason = "mirrors google.protobuf.Value protobuf wire variant names"
    )]
    pub(crate) enum Kind {
        /// JSON `null`; the numeric value is unused.
        #[prost(enumeration = "super::PbNull", tag = "1")]
        NullValue(i32),
        /// A JSON number, always a double on the wire.
        #[prost(double, tag = "2")]
        NumberValue(f64),
        /// A JSON string.
        #[prost(string, tag = "3")]
        StringValue(String),
        /// A JSON boolean.
        #[prost(bool, tag = "4")]
        BoolValue(bool),
        /// A JSON object.
        #[prost(message, tag = "5")]
        StructValue(super::PbStruct),
        /// A JSON array.
        #[prost(message, tag = "6")]
        ListValue(super::PbList),
    }
}

/// The `NULL_VALUE` of `google.protobuf.NullValue`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub(crate) enum PbNull {
    /// The only null value.
    NullValue = 0,
}

/// A `google.protobuf.Struct`: string keys to [`PbValue`].
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct PbStruct {
    /// The object fields, unordered on the wire.
    #[prost(btree_map = "string, message", tag = "1")]
    pub(crate) fields: BTreeMap<String, PbValue>,
}

/// A `google.protobuf.ListValue`: an ordered list of [`PbValue`].
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct PbList {
    /// The array items in order.
    #[prost(message, repeated, tag = "1")]
    pub(crate) values: Vec<PbValue>,
}

/// One `AgentClientMessage`: the framed client-to-server envelope content.
///
/// Oneof arms: run request 1, exec reply 2, exec control 5, KV reply 3,
/// conversation action 4, interaction response 6, heartbeat 7.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AgentClientMessage {
    /// The message; absent is never sent.
    #[prost(oneof = "client_message::Body", tags = "1, 2, 5, 3, 4, 6, 7")]
    pub(crate) message: Option<client_message::Body>,
}

/// Nested arms of [`AgentClientMessage`].
pub(crate) mod client_message {
    /// One client message arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    #[expect(
        clippy::large_enum_variant,
        reason = "prost Oneof representation mirrors Cursor protobuf message"
    )]
    pub(crate) enum Body {
        /// Opens or continues a run; the first frame of the request body.
        #[prost(message, tag = "1")]
        RunRequest(super::AgentRunRequest),
        /// One tool-call answer for an exec frame.
        #[prost(message, tag = "2")]
        ExecClientMessage(super::ExecClientMessage),
        /// Throw, stream close, or heartbeat for one exec frame.
        #[prost(message, tag = "5")]
        ExecClientControlMessage(super::ExecClientControlMessage),
        /// One blob answer for a KV frame.
        #[prost(message, tag = "3")]
        KvClientMessage(super::KvClientMessage),
        /// A turn-level action (used for cancel).
        #[prost(message, tag = "4")]
        ConversationAction(super::ConversationAction),
        /// One permission-query answer.
        #[prost(message, tag = "6")]
        InteractionResponse(super::InteractionResponse),
        /// The 5 s run keepalive.
        #[prost(message, tag = "7")]
        ClientHeartbeat(super::ClientHeartbeat),
    }
}

/// One `AgentRunRequest`: full history plus the opening action.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AgentRunRequest {
    /// History blobs: system records, then prior turns.
    #[prost(message, tag = "1")]
    pub(crate) conversation_state: Option<ConversationStateStructure>,
    /// The user message or resume that opens the turn.
    #[prost(message, tag = "2")]
    pub(crate) action: Option<ConversationAction>,
    /// The legacy model identity.
    #[prost(message, tag = "3")]
    pub(crate) model_details: Option<ModelDetails>,
    /// The rich model identity the server routes on.
    #[prost(message, tag = "9")]
    pub(crate) requested_model: Option<RequestedModel>,
    /// The advertised MCP tools.
    #[prost(message, tag = "4")]
    pub(crate) mcp_tools: Option<McpTools>,
    /// The conversation id; minted per run, never reused across runs.
    #[prost(string, optional, tag = "5")]
    pub(crate) conversation_id: Option<String>,
}

/// One `ConversationStateStructure`: history as content-addressed blobs.
///
/// Only the two history fields are modeled; every other field the server
/// sends (todos, file states, summaries, timings) decodes into nothing, and
/// requests never send them.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ConversationStateStructure {
    /// System and history records as `rootPromptMessagesJson` blob ids.
    #[prost(bytes, repeated, tag = "1")]
    pub(crate) root_prompt_messages_json: Vec<Vec<u8>>,
    /// Display turns as `ConversationTurnStructure` blob ids.
    #[prost(bytes, repeated, tag = "8")]
    pub(crate) turns: Vec<Vec<u8>>,
}

/// One `ConversationAction`: the turn opener.
///
/// Oneof arms: user message 1, resume 2, cancel 3. Plan, shell, summarize,
/// and ask-question-completion actions are never sent by this client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ConversationAction {
    /// The action; absent is never sent.
    #[prost(oneof = "conversation_action::Action", tags = "1, 2, 3")]
    pub(crate) action: Option<conversation_action::Action>,
}

/// Nested arms of [`ConversationAction`].
pub(crate) mod conversation_action {
    /// One conversation action arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    #[expect(
        clippy::large_enum_variant,
        reason = "prost Oneof representation mirrors Cursor protobuf message"
    )]
    #[expect(
        clippy::enum_variant_names,
        reason = "mirrors Cursor protobuf wire variant names"
    )]
    pub(crate) enum Action {
        /// A fresh user message opens the turn.
        #[prost(message, tag = "1")]
        UserMessageAction(super::UserMessageAction),
        /// History only; no new user message.
        #[prost(message, tag = "2")]
        ResumeAction(super::ResumeAction),
        /// Aborts the run.
        #[prost(message, tag = "3")]
        CancelAction(super::CancelAction),
    }
}

/// A `UserMessageAction`: the active user message plus request context.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct UserMessageAction {
    /// The message text, id, and images.
    #[prost(message, tag = "1")]
    pub(crate) user_message: Option<UserMessage>,
    /// Rules and tools the server builds the prompt from.
    #[prost(message, tag = "2")]
    pub(crate) request_context: Option<RequestContext>,
}

/// A `ResumeAction`: history only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ResumeAction {}

/// A `CancelAction`: aborts the run.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CancelAction {}

/// A `UserMessage`: text, stable id, and optional images.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct UserMessage {
    /// The message text.
    #[prost(string, tag = "1")]
    pub(crate) text: String,
    /// The message id, minted per request.
    #[prost(string, tag = "2")]
    pub(crate) message_id: String,
    /// Attached images, when any.
    #[prost(message, tag = "3")]
    pub(crate) selected_context: Option<SelectedContext>,
    /// The conversation mode; zero sends the server default.
    #[prost(int32, tag = "4")]
    pub(crate) mode: i32,
}

/// A `SelectedContext` carrying only images; every other selection kind is
/// never sent by this client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct SelectedContext {
    /// The attached images in order.
    #[prost(message, repeated, tag = "1")]
    pub(crate) selected_images: Vec<SelectedImage>,
}

/// A `SelectedImage` with inline bytes; blob-id forms are never sent.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct SelectedImage {
    /// The image id, minted per request.
    #[prost(string, tag = "2")]
    pub(crate) uuid: String,
    /// The image media type.
    #[prost(string, tag = "7")]
    pub(crate) mime_type: String,
    /// The inline bytes.
    #[prost(oneof = "selected_image::Data", tags = "8")]
    pub(crate) data_or_blob_id: Option<selected_image::Data>,
}

/// Nested arms of [`SelectedImage`].
pub(crate) mod selected_image {
    /// The inline-bytes arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Data {
        /// Raw image bytes.
        #[prost(bytes, tag = "8")]
        Data(Vec<u8>),
    }
}

/// A `RequestContext`: the rules and tools the server builds the prompt
/// from. The exec handshake asks for this separately; the run request may
/// also carry it.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct RequestContext {
    /// Always-apply rules derived from the system prompt.
    #[prost(message, repeated, tag = "2")]
    pub(crate) rules: Vec<CursorRule>,
    /// Host environment facts; unset sends the server defaults.
    #[prost(message, tag = "4")]
    pub(crate) env: Option<RequestContextEnv>,
    /// The advertised MCP tools.
    #[prost(message, repeated, tag = "7")]
    pub(crate) tools: Vec<McpToolDefinition>,
    /// Inline file content by path; empty in this client.
    #[prost(btree_map = "string, string", tag = "20")]
    pub(crate) file_contents: BTreeMap<String, String>,
    /// Whether hosted web search may run; unset leaves the server default.
    #[prost(bool, optional, tag = "17")]
    pub(crate) web_search_enabled: Option<bool>,
}

/// One `CursorRule`: a system-prompt entry as a global always-apply rule.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CursorRule {
    /// A stable synthetic path naming the prompt entry.
    #[prost(string, tag = "1")]
    pub(crate) full_path: String,
    /// The rule text.
    #[prost(string, tag = "2")]
    pub(crate) content: String,
    /// Always the global kind in this client.
    #[prost(message, tag = "3")]
    pub(crate) rule_type: Option<CursorRuleType>,
    /// Always `USER` in this client.
    #[prost(int32, tag = "4")]
    pub(crate) source: i32,
}

/// The `CursorRuleType` oneof: only the global arm is modeled.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CursorRuleType {
    /// The rule kind; only global is ever sent.
    #[prost(oneof = "cursor_rule_type::Kind", tags = "1")]
    pub(crate) rule_type: Option<cursor_rule_type::Kind>,
}

/// Nested arms of [`CursorRuleType`].
pub(crate) mod cursor_rule_type {
    /// The global always-apply arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Kind {
        /// An always-apply rule.
        #[prost(message, tag = "1")]
        Global(super::CursorRuleTypeGlobal),
    }
}

/// A `CursorRuleTypeGlobal`: no fields.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CursorRuleTypeGlobal {}

/// `CursorRuleSource` values.
pub(crate) struct CursorRuleSource;

impl CursorRuleSource {
    /// User rules; every rule this client sends.
    pub(crate) const USER: i32 = 2;
}

/// A `RequestContextEnv`: host facts for the system prompt. Every field
/// sends the server default when unset.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct RequestContextEnv {
    /// OS version text.
    #[prost(string, tag = "1")]
    pub(crate) os_version: String,
    /// Workspace root paths.
    #[prost(string, repeated, tag = "2")]
    pub(crate) workspace_paths: Vec<String>,
    /// The login shell.
    #[prost(string, tag = "3")]
    pub(crate) shell: String,
    /// Whether the sandbox is on.
    #[prost(bool, tag = "5")]
    pub(crate) sandbox_enabled: bool,
    /// Time zone name.
    #[prost(string, tag = "10")]
    pub(crate) time_zone: String,
    /// The project folder.
    #[prost(string, tag = "11")]
    pub(crate) project_folder: String,
}

/// A `ModelDetails`: the legacy model identity the server validates
/// independently of [`RequestedModel`].
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ModelDetails {
    /// The account-usable model slug.
    #[prost(string, tag = "1")]
    pub(crate) model_id: String,
    /// The display model id.
    #[prost(string, tag = "3")]
    pub(crate) display_model_id: String,
    /// The display name.
    #[prost(string, tag = "4")]
    pub(crate) display_name: String,
    /// The short display name.
    #[prost(string, tag = "5")]
    pub(crate) display_name_short: String,
    /// Model aliases.
    #[prost(string, repeated, tag = "6")]
    pub(crate) aliases: Vec<String>,
    /// Thinking support marker; set when the route reasons.
    #[prost(message, tag = "2")]
    pub(crate) thinking_details: Option<ThinkingDetails>,
    /// Whether the route needs max mode.
    #[prost(bool, optional, tag = "7")]
    pub(crate) max_mode: Option<bool>,
}

/// A `ThinkingDetails` marker: no fields.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ThinkingDetails {}

/// A `RequestedModel`: the rich identity the server routes on.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct RequestedModel {
    /// The base model id plus parameter values.
    #[prost(string, tag = "1")]
    pub(crate) model_id: String,
    /// Whether the route needs max mode.
    #[prost(bool, tag = "2")]
    pub(crate) max_mode: bool,
    /// Route parameters such as reasoning effort.
    #[prost(message, repeated, tag = "3")]
    pub(crate) parameters: Vec<RequestedModelParameter>,
}

/// One `RequestedModel_ModelParameterbytes`: a string-valued parameter.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct RequestedModelParameter {
    /// The parameter id.
    #[prost(string, tag = "1")]
    pub(crate) id: String,
    /// The parameter value, always a string.
    #[prost(string, tag = "2")]
    pub(crate) value: String,
}

/// An `McpTools` list: the advertised tools.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpTools {
    /// The tool definitions in registration order.
    #[prost(message, repeated, tag = "1")]
    pub(crate) tools: Vec<McpToolDefinition>,
}

/// An `McpToolDefinition`: one dal tool advertised over the exec channel.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpToolDefinition {
    /// The wire tool name.
    #[prost(string, tag = "1")]
    pub(crate) name: String,
    /// Always `dal-agent` in this client.
    #[prost(string, tag = "4")]
    pub(crate) provider_identifier: String,
    /// The wire tool name, as the server echoes it back.
    #[prost(string, tag = "5")]
    pub(crate) tool_name: String,
    /// The tool description.
    #[prost(string, tag = "2")]
    pub(crate) description: String,
    /// The JSON schema encoded as a `google.protobuf.Value`.
    #[prost(bytes, tag = "3")]
    pub(crate) input_schema: Vec<u8>,
}

/// One `AgentServerMessage`: the framed server-to-client envelope content.
///
/// Oneof arms: interaction update 1, exec request 2, exec control 5,
/// checkpoint 3, KV request 4, interaction query 7.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AgentServerMessage {
    /// The message; absent frames are ignored.
    #[prost(oneof = "server_message::Body", tags = "1, 2, 5, 3, 4, 7")]
    pub(crate) message: Option<server_message::Body>,
}

/// Nested arms of [`AgentServerMessage`].
pub(crate) mod server_message {
    /// One server message arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Body {
        /// A stream delta or terminal update.
        #[prost(message, tag = "1")]
        InteractionUpdate(super::InteractionUpdate),
        /// A tool or context request the client must answer.
        #[prost(message, tag = "2")]
        ExecServerMessage(super::ExecServerMessage),
        /// An exec abort for one numeric exec id.
        #[prost(message, tag = "5")]
        ExecServerControlMessage(super::ExecServerControlMessage),
        /// The server's record chain for capture and resume.
        #[prost(message, tag = "3")]
        Checkpoint(super::ConversationStateStructure),
        /// A blob-store get or set.
        #[prost(message, tag = "4")]
        KvServerMessage(super::KvServerMessage),
        /// A permission query the client must answer.
        #[prost(message, tag = "7")]
        InteractionQuery(super::InteractionQuery),
    }
}

/// One `InteractionUpdate`: the streaming vocabulary.
///
/// Modeled arms: text delta 1, partial tool call 7, tool-call delta 15,
/// tool-call started 2, tool-call completed 3, thinking delta 4, thinking
/// completed 5, user message appended 6, token delta 8, heartbeat 13, turn
/// ended 14, step started 16, step completed 17. Summary and shell-output
/// updates decode into nothing; the session reads an update that decodes into
/// nothing as unmodeled progress, never as a keepalive, so the heartbeat has
/// its own arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct InteractionUpdate {
    /// The update; absent updates are ignored.
    #[prost(
        oneof = "interaction_update::Update",
        tags = "1, 7, 15, 2, 3, 4, 5, 6, 8, 13, 14, 16, 17"
    )]
    pub(crate) update: Option<interaction_update::Update>,
}

/// Nested arms of [`InteractionUpdate`].
pub(crate) mod interaction_update {
    /// One interaction update arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Update {
        /// Visible text.
        #[prost(message, tag = "1")]
        TextDelta(super::TextDeltaUpdate),
        /// Cumulative argument snapshot for one call.
        #[prost(message, tag = "7")]
        PartialToolCall(super::PartialToolCallUpdate),
        /// Incremental argument delta for one call.
        #[prost(message, tag = "15")]
        ToolCallDelta(super::ToolCallDeltaUpdate),
        /// A tool call opened.
        #[prost(message, tag = "2")]
        ToolCallStarted(super::ToolCallStartedUpdate),
        /// A tool call closed with its authoritative call.
        #[prost(message, tag = "3")]
        ToolCallCompleted(super::ToolCallCompletedUpdate),
        /// Reasoning text.
        #[prost(message, tag = "4")]
        ThinkingDelta(super::ThinkingDeltaUpdate),
        /// Reasoning finished; carries no text.
        #[prost(message, tag = "5")]
        ThinkingCompleted(super::ThinkingCompletedUpdate),
        /// Server echo of the opening user message; ignored.
        #[prost(message, tag = "6")]
        UserMessageAppended(super::UserMessageAppendedUpdate),
        /// A running output-token estimate; informational only.
        #[prost(message, tag = "8")]
        TokenDelta(super::TokenDeltaUpdate),
        /// The server keepalive: no content, no state change.
        #[prost(message, tag = "13")]
        Heartbeat(super::HeartbeatUpdate),
        /// The turn's terminal update with authoritative usage.
        #[prost(message, tag = "14")]
        TurnEnded(super::TurnEndedUpdate),
        /// A conversation step opened.
        #[prost(message, tag = "16")]
        StepStarted(super::StepStartedUpdate),
        /// A conversation step closed.
        #[prost(message, tag = "17")]
        StepCompleted(super::StepCompletedUpdate),
    }
}

/// A `TextDeltaUpdate`: visible text.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct TextDeltaUpdate {
    /// The next text slice.
    #[prost(string, tag = "1")]
    pub(crate) text: String,
}

/// A `ThinkingDeltaUpdate`: reasoning text.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ThinkingDeltaUpdate {
    /// The next reasoning slice.
    #[prost(string, tag = "1")]
    pub(crate) text: String,
}

/// A `ThinkingCompletedUpdate`: reasoning finished.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ThinkingCompletedUpdate {}

/// A `HeartbeatUpdate`: the server keepalive.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct HeartbeatUpdate {}

/// A `TokenDeltaUpdate`: a running output estimate.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct TokenDeltaUpdate {
    /// Tokens since the last delta.
    #[prost(int32, tag = "1")]
    pub(crate) tokens: i32,
}

/// A `StepStartedUpdate`: a step opened.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct StepStartedUpdate {
    /// The step id.
    #[prost(uint64, tag = "1")]
    pub(crate) step_id: u64,
}

/// A `StepCompletedUpdate`: a step closed.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct StepCompletedUpdate {
    /// The step id.
    #[prost(uint64, tag = "1")]
    pub(crate) step_id: u64,
}

/// A `UserMessageAppendedUpdate`: server echo, ignored.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct UserMessageAppendedUpdate {}

/// A `TurnEndedUpdate`: the terminal update with usage.
#[derive(Clone, PartialEq, prost::Message)]
#[expect(
    clippy::struct_field_names,
    reason = "mirrors Cursor protobuf wire field names"
)]
pub(crate) struct TurnEndedUpdate {
    /// Whole-prompt input tokens, cache hits and writes included.
    #[prost(int64, optional, tag = "1")]
    pub(crate) input_tokens: Option<i64>,
    /// Output tokens.
    #[prost(int64, optional, tag = "2")]
    pub(crate) output_tokens: Option<i64>,
    /// Input tokens served from cache, a subset of the input.
    #[prost(int64, optional, tag = "3")]
    pub(crate) cache_read_tokens: Option<i64>,
    /// Input tokens written to cache.
    #[prost(int64, optional, tag = "4")]
    pub(crate) cache_write_tokens: Option<i64>,
    /// Reasoning tokens, separately reported.
    #[prost(int64, optional, tag = "5")]
    pub(crate) reasoning_tokens: Option<i64>,
}

/// A `ToolCallStartedUpdate`: a tool call opened.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ToolCallStartedUpdate {
    /// The envelope call id used for correlation.
    #[prost(string, tag = "1")]
    pub(crate) call_id: String,
    /// The announced call, if the frame carries one.
    #[prost(message, tag = "2")]
    pub(crate) tool_call: Option<ToolCall>,
}

/// A `PartialToolCallUpdate`: a cumulative argument snapshot.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct PartialToolCallUpdate {
    /// The envelope call id used for correlation.
    #[prost(string, tag = "1")]
    pub(crate) call_id: String,
    /// The call as known so far, if the frame carries one.
    #[prost(message, tag = "2")]
    pub(crate) tool_call: Option<ToolCall>,
    /// Aggregated argument text so far as JSON text; a snapshot, not a
    /// fragment.
    #[prost(string, tag = "3")]
    pub(crate) args_text_delta: String,
}

/// A `ToolCallCompletedUpdate`: a tool call closed.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ToolCallCompletedUpdate {
    /// The envelope call id used for correlation.
    #[prost(string, tag = "1")]
    pub(crate) call_id: String,
    /// The authoritative call, if the frame carries one.
    #[prost(message, tag = "2")]
    pub(crate) tool_call: Option<ToolCall>,
}

/// A `ToolCallDeltaUpdate`: an incremental argument delta. The delta payload
/// itself is never read (edit and shell stream deltas are display-only in
/// this client); only the call id routes.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ToolCallDeltaUpdate {
    /// The envelope call id used for correlation.
    #[prost(string, tag = "1")]
    pub(crate) call_id: String,
}

/// A `ToolCall`: the envelope id plus, for MCP calls, the typed call.
///
/// MCP calls (15) route to dal. Repository connector calls (68) are decoded
/// so the run can refuse their account-level effects. Other native and
/// hosted arms are ignored here; native execution uses the exec channel.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ToolCall {
    /// The dal-routed call or repository connector announcement.
    #[prost(oneof = "tool_call::Tool", tags = "15, 68")]
    pub(crate) tool: Option<tool_call::Tool>,
    /// The call id carried on the envelope.
    #[prost(string, optional, tag = "57")]
    pub(crate) tool_call_id: Option<String>,
}

/// Nested arms of [`ToolCall`].
pub(crate) mod tool_call {
    /// The modeled arms: dal-routed calls plus the server-side connector
    /// the run refuses to host.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Tool {
        /// A dal-routed tool call.
        #[prost(message, tag = "15")]
        McpToolCall(super::McpToolCall),
        /// The server-side repository connector: acting on the account
        /// without a permission channel, so its announcement loses the run.
        #[prost(message, tag = "68")]
        ConnectScm(super::OpaqueToolCall),
    }
}

/// An opaque non-MCP tool call: the arm tag is the whole signal.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct OpaqueToolCall {}

/// An `McpToolCall`: arguments now, result never (the exec channel owns
/// results; the completion frame only refreshes arguments).
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpToolCall {
    /// The call arguments.
    #[prost(message, tag = "1")]
    pub(crate) args: Option<McpArgs>,
}

/// An `McpArgs`: one dal tool invocation.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpArgs {
    /// The wire tool name.
    #[prost(string, tag = "1")]
    pub(crate) name: String,
    /// Per-argument values, each a `google.protobuf.Value`.
    #[prost(btree_map = "string, bytes", tag = "2")]
    pub(crate) args: BTreeMap<String, Vec<u8>>,
    /// The call id the result pairs under.
    #[prost(string, tag = "3")]
    pub(crate) tool_call_id: String,
    /// The wire tool name, as the server echoes it.
    #[prost(string, tag = "5")]
    pub(crate) tool_name: String,
    /// True for approval probes, which are never invocations.
    #[prost(bool, tag = "7")]
    pub(crate) smart_mode_approval_only: bool,
    /// Whether the server believes approval was skipped.
    #[prost(bool, tag = "8")]
    pub(crate) skip_approval: bool,
    /// The provider that advertised the tool; history calls carry the
    /// identifier the request advertised so they pair with those tools.
    #[prost(string, tag = "4")]
    pub(crate) provider_identifier: String,
    /// The MCP server identifier.
    #[prost(string, tag = "9")]
    pub(crate) server_identifier: String,
}

/// One `ExecServerMessage`: a request the client must answer.
///
/// Modeled arms: request context 10, MCP call 11, shell 2, shell stream 14,
/// write 3, delete 4, grep 5, read 7, ls 8, diagnostics 9, fetch 20,
/// list-MCP-resources 17, read-MCP-resource 18, MCP state 36, and the three
/// allowlist prechecks 41-43. Every other arm (background shells, hooks,
/// subagents, computer use, record screen, git diff, conversation search,
/// and the `pi_*` CLI tools) decodes into nothing and is refused as
/// unserved. Numeric id 1 plus `exec_id` 15 route the reply.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExecServerMessage {
    /// The numeric exec id the reply echoes.
    #[prost(uint32, tag = "1")]
    pub(crate) id: u32,
    /// The attachable execution id, echoed when present.
    #[prost(string, tag = "15")]
    pub(crate) exec_id: String,
    /// The request; absent frames are refused as unknown.
    #[prost(
        oneof = "exec_server_message::Body",
        tags = "10, 11, 2, 14, 3, 4, 5, 7, 8, 9, 20, 17, 18, 36, 41, 42, 43"
    )]
    pub(crate) body: Option<exec_server_message::Body>,
}

/// Nested arms of [`ExecServerMessage`].
pub(crate) mod exec_server_message {
    /// One exec request arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Body {
        /// The server asks for rules and tools.
        #[prost(message, tag = "10")]
        RequestContextArgs(super::RequestContextArgs),
        /// One dal tool invocation.
        #[prost(message, tag = "11")]
        McpArgs(super::McpArgs),
        /// A native shell run; refused, never executed.
        #[prost(message, tag = "2")]
        ShellArgs(super::ShellArgs),
        /// A native streaming shell run; refused, never executed.
        #[prost(message, tag = "14")]
        ShellStreamArgs(super::ShellArgs),
        /// A native file write; refused, never executed.
        #[prost(message, tag = "3")]
        WriteArgs(super::WriteArgs),
        /// A native file delete; refused, never executed.
        #[prost(message, tag = "4")]
        DeleteArgs(super::DeleteArgs),
        /// A native grep; refused, never executed.
        #[prost(message, tag = "5")]
        GrepArgs(super::GrepArgs),
        /// A native file read; refused, never executed.
        #[prost(message, tag = "7")]
        ReadArgs(super::ReadArgs),
        /// A native directory listing; refused, never executed.
        #[prost(message, tag = "8")]
        LsArgs(super::LsArgs),
        /// Native diagnostics; refused, never executed.
        #[prost(message, tag = "9")]
        DiagnosticsArgs(super::DiagnosticsArgs),
        /// A hosted fetch; refused, never fetched.
        #[prost(message, tag = "20")]
        FetchArgs(super::FetchArgs),
        /// An MCP resource listing; answered empty.
        #[prost(message, tag = "17")]
        ListMcpResourcesArgs(super::ListMcpResourcesExecArgs),
        /// An MCP resource read; answered not-found.
        #[prost(message, tag = "18")]
        ReadMcpResourceArgs(super::ReadMcpResourceExecArgs),
        /// The MCP server inventory; answered empty.
        #[prost(message, tag = "36")]
        McpStateArgs(super::McpStateExecArgs),
        /// A shell precheck; answered not-allowlisted.
        #[prost(message, tag = "41")]
        ShellAllowlistPrecheck(super::ShellAllowlistPrecheckArgs),
        /// An MCP precheck; answered not-allowlisted.
        #[prost(message, tag = "42")]
        McpAllowlistPrecheck(super::McpAllowlistPrecheckArgs),
        /// A web-fetch precheck; answered not-allowlisted.
        #[prost(message, tag = "43")]
        WebFetchAllowlistPrecheck(super::WebFetchAllowlistPrecheckArgs),
    }
}

/// `RequestContextArgs`: the server asks for rules and tools.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct RequestContextArgs {}

/// `ShellArgs`: command plus working directory.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ShellArgs {
    /// The command the server wanted run.
    #[prost(string, tag = "1")]
    pub(crate) command: String,
    /// The working directory the server named.
    #[prost(string, tag = "2")]
    pub(crate) working_directory: String,
}

/// `WriteArgs`: path only (content is never read).
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct WriteArgs {
    /// The path the server wanted written.
    #[prost(string, tag = "1")]
    pub(crate) path: String,
}

/// `DeleteArgs`: path only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DeleteArgs {
    /// The path the server wanted deleted.
    #[prost(string, tag = "1")]
    pub(crate) path: String,
}

/// `GrepArgs`: pattern only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GrepArgs {
    /// The pattern the server wanted searched.
    #[prost(string, tag = "1")]
    pub(crate) pattern: String,
}

/// `ReadArgs`: path only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ReadArgs {
    /// The path the server wanted read.
    #[prost(string, tag = "1")]
    pub(crate) path: String,
}

/// `LsArgs`: path only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct LsArgs {
    /// The path the server wanted listed.
    #[prost(string, tag = "1")]
    pub(crate) path: String,
}

/// `DiagnosticsArgs`: path only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DiagnosticsArgs {
    /// The path the server wanted diagnostics for.
    #[prost(string, tag = "1")]
    pub(crate) path: String,
}

/// `FetchArgs`: URL only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct FetchArgs {
    /// The URL the server wanted fetched.
    #[prost(string, tag = "1")]
    pub(crate) url: String,
}

/// `ListMcpResourcesExecArgs`: optional server filter.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ListMcpResourcesExecArgs {
    /// The server name filter, when set.
    #[prost(string, optional, tag = "1")]
    pub(crate) server: Option<String>,
}

/// `ReadMcpResourceExecArgs`: server plus URI.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ReadMcpResourceExecArgs {
    /// The server name.
    #[prost(string, tag = "1")]
    pub(crate) server: String,
    /// The resource URI.
    #[prost(string, tag = "2")]
    pub(crate) uri: String,
}

/// `McpStateExecArgs`: no fields are read.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpStateExecArgs {}

/// `ShellAllowlistPrecheckArgs`: no fields are read.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ShellAllowlistPrecheckArgs {}

/// `McpAllowlistPrecheckArgs`: no fields are read.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpAllowlistPrecheckArgs {}

/// `WebFetchAllowlistPrecheckArgs`: no fields are read.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct WebFetchAllowlistPrecheckArgs {}

/// One `ExecClientMessage`: a tool-call answer.
///
/// Only the arms this client sends are modeled: request-context result 10,
/// MCP result 11, shell result 2, shell stream 14, write 3, delete 4, grep
/// 5, read 7, ls 8, diagnostics 9, fetch 20, list-resources 17,
/// read-resource 18, MCP state 36, and the three allowlist results 41-43.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExecClientMessage {
    /// The numeric exec id being answered.
    #[prost(uint32, tag = "1")]
    pub(crate) id: u32,
    /// The attachable execution id, echoed when the request carried one.
    #[prost(string, tag = "15")]
    pub(crate) exec_id: String,
    /// The answer; absent is never sent.
    #[prost(
        oneof = "exec_client_message::Body",
        tags = "10, 11, 2, 14, 3, 4, 5, 7, 8, 9, 20, 17, 18, 36, 41, 42, 43"
    )]
    pub(crate) message: Option<exec_client_message::Body>,
}

/// Nested arms of [`ExecClientMessage`].
pub(crate) mod exec_client_message {
    /// One exec answer arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Body {
        /// The rules-and-tools answer.
        #[prost(message, tag = "10")]
        RequestContextResult(super::RequestContextResult),
        /// A dal tool result.
        #[prost(message, tag = "11")]
        McpResult(super::McpResult),
        /// A shell refusal.
        #[prost(message, tag = "2")]
        ShellResult(super::ShellResult),
        /// A shell-stream refusal plus exit.
        #[prost(message, tag = "14")]
        ShellStream(super::ShellStream),
        /// A write refusal.
        #[prost(message, tag = "3")]
        WriteResult(super::WriteResult),
        /// A delete refusal.
        #[prost(message, tag = "4")]
        DeleteResult(super::DeleteResult),
        /// A grep error.
        #[prost(message, tag = "5")]
        GrepResult(super::GrepResult),
        /// A read refusal.
        #[prost(message, tag = "7")]
        ReadResult(super::ReadResult),
        /// An ls refusal.
        #[prost(message, tag = "8")]
        LsResult(super::LsResult),
        /// A diagnostics refusal.
        #[prost(message, tag = "9")]
        DiagnosticsResult(super::DiagnosticsResult),
        /// A fetch error.
        #[prost(message, tag = "20")]
        FetchResult(super::FetchResult),
        /// An empty resource listing.
        #[prost(message, tag = "17")]
        ListMcpResourcesResult(super::ListMcpResourcesExecResult),
        /// A resource not-found.
        #[prost(message, tag = "18")]
        ReadMcpResourceResult(super::ReadMcpResourceExecResult),
        /// An empty MCP inventory.
        #[prost(message, tag = "36")]
        McpStateResult(super::McpStateExecResult),
        /// A shell precheck denial.
        #[prost(message, tag = "41")]
        ShellAllowlistResult(super::ShellAllowlistPrecheckResult),
        /// An MCP precheck denial.
        #[prost(message, tag = "42")]
        McpAllowlistResult(super::McpAllowlistPrecheckResult),
        /// A web-fetch precheck denial.
        #[prost(message, tag = "43")]
        WebFetchAllowlistResult(super::WebFetchAllowlistPrecheckResult),
    }
}

/// A `RequestContextResult`: success only in this client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct RequestContextResult {
    /// The result; only success is ever sent.
    #[prost(oneof = "request_context_result::Result", tags = "1")]
    pub(crate) result: Option<request_context_result::Result>,
}

/// Nested arms of [`RequestContextResult`].
pub(crate) mod request_context_result {
    /// The success arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// Rules and tools.
        #[prost(message, tag = "1")]
        Success(super::RequestContextSuccess),
    }
}

/// A `RequestContextSuccess`: the rules and tools.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct RequestContextSuccess {
    /// The context.
    #[prost(message, tag = "1")]
    pub(crate) request_context: Option<RequestContext>,
}

/// An `McpResult`: success, error, or one of the typed denials.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpResult {
    /// The result; absent is never sent.
    #[prost(oneof = "mcp_result::Result", tags = "1, 2, 3, 4, 5")]
    pub(crate) result: Option<mcp_result::Result>,
}

/// Nested arms of [`McpResult`].
pub(crate) mod mcp_result {
    /// One MCP result arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// The tool ran.
        #[prost(message, tag = "1")]
        Success(super::McpSuccess),
        /// The tool ran and failed.
        #[prost(message, tag = "2")]
        Error(super::McpError),
        /// The call was refused without running.
        #[prost(message, tag = "3")]
        Rejected(super::McpRejected),
        /// The tool is unknown on this host.
        #[prost(message, tag = "5")]
        ToolNotFound(super::McpToolNotFound),
        /// The server is unknown on this host.
        #[prost(message, tag = "6")]
        ServerNotFound(super::McpServerNotFound),
    }
}

/// An `McpSuccess`: content plus the error flag.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpSuccess {
    /// Text and image items in order.
    #[prost(message, repeated, tag = "1")]
    pub(crate) content: Vec<McpContentItem>,
    /// True when the content reports a tool error.
    #[prost(bool, tag = "2")]
    pub(crate) is_error: bool,
}

/// One `McpToolResultContentItem`: text or image.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpContentItem {
    /// The item; absent is never sent.
    #[prost(oneof = "mcp_content_item::Content", tags = "1, 2")]
    pub(crate) content: Option<mcp_content_item::Content>,
}

/// Nested arms of [`McpContentItem`].
pub(crate) mod mcp_content_item {
    /// One content arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Content {
        /// UTF-8 text.
        #[prost(message, tag = "1")]
        Text(super::McpTextContent),
        /// Raw image bytes.
        #[prost(message, tag = "2")]
        Image(super::McpImageContent),
    }
}

/// An `McpTextContent`: text only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpTextContent {
    /// The text.
    #[prost(string, tag = "1")]
    pub(crate) text: String,
}

/// An `McpImageContent`: bytes plus media type.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpImageContent {
    /// The raw image bytes.
    #[prost(bytes, tag = "1")]
    pub(crate) data: Vec<u8>,
    /// The image media type.
    #[prost(string, tag = "2")]
    pub(crate) mime_type: String,
}

/// An `McpError`: the tool ran and failed.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpError {
    /// The failure text.
    #[prost(string, tag = "1")]
    pub(crate) error: String,
}

/// An `McpRejected`: refused without running.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpRejected {
    /// Why the call was refused.
    #[prost(string, tag = "1")]
    pub(crate) reason: String,
}

/// An `McpToolNotFound`: unknown tool name.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpToolNotFound {
    /// The unknown name.
    #[prost(string, tag = "1")]
    pub(crate) name: String,
    /// The advertised wire names.
    #[prost(string, repeated, tag = "2")]
    pub(crate) available_tools: Vec<String>,
}

/// An `McpServerNotFound`: unknown server identifier.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpServerNotFound {
    /// The unknown server identifier.
    #[prost(string, tag = "1")]
    pub(crate) name: String,
}

/// A `ShellResult` carrying only the refusal arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ShellResult {
    /// The result; only refusal is ever sent.
    #[prost(oneof = "shell_result::Result", tags = "4")]
    pub(crate) result: Option<shell_result::Result>,
}

/// Nested arms of [`ShellResult`].
pub(crate) mod shell_result {
    /// The refusal arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// The shell run was refused without running.
        #[prost(message, tag = "4")]
        Rejected(super::ShellRejected),
    }
}

/// A `ShellRejected`: command, directory, and reason.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ShellRejected {
    /// The refused command.
    #[prost(string, tag = "1")]
    pub(crate) command: String,
    /// The working directory the server named.
    #[prost(string, tag = "2")]
    pub(crate) working_directory: String,
    /// Why the run was refused.
    #[prost(string, tag = "3")]
    pub(crate) reason: String,
    /// Always false in this client.
    #[prost(bool, tag = "4")]
    pub(crate) is_readonly: bool,
}

/// A `ShellStream` carrying a refusal followed by an exit.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ShellStream {
    /// The event; refusal and exit are the only ones sent.
    #[prost(oneof = "shell_stream::Event", tags = "5, 3")]
    pub(crate) event: Option<shell_stream::Event>,
}

/// Nested arms of [`ShellStream`].
pub(crate) mod shell_stream {
    /// Refusal and exit arms.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Event {
        /// The streaming run was refused without running.
        #[prost(message, tag = "5")]
        Rejected(super::ShellRejected),
        /// The stream ends with a failure exit.
        #[prost(message, tag = "3")]
        Exit(super::ShellStreamExit),
    }
}

/// A `ShellStreamExit`: code, directory, and abort flag.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ShellStreamExit {
    /// The exit code; 1 for a refusal.
    #[prost(uint32, tag = "1")]
    pub(crate) code: u32,
    /// The working directory the server named.
    #[prost(string, tag = "2")]
    pub(crate) cwd: String,
    /// Always false in this client.
    #[prost(bool, tag = "4")]
    pub(crate) aborted: bool,
}

/// A `WriteResult` carrying only the refusal arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct WriteResult {
    /// The result; only refusal is ever sent.
    #[prost(oneof = "write_result::Result", tags = "6")]
    pub(crate) result: Option<write_result::Result>,
}

/// Nested arms of [`WriteResult`].
pub(crate) mod write_result {
    /// The refusal arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// The write was refused without running.
        #[prost(message, tag = "6")]
        Rejected(super::WriteRejected),
    }
}

/// A `WriteRejected`: path and reason.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct WriteRejected {
    /// The refused path.
    #[prost(string, tag = "1")]
    pub(crate) path: String,
    /// Why the write was refused.
    #[prost(string, tag = "2")]
    pub(crate) reason: String,
}

/// A `DeleteResult` carrying only the refusal arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DeleteResult {
    /// The result; only refusal is ever sent.
    #[prost(oneof = "delete_result::Result", tags = "6")]
    pub(crate) result: Option<delete_result::Result>,
}

/// Nested arms of [`DeleteResult`].
pub(crate) mod delete_result {
    /// The refusal arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// The delete was refused without running.
        #[prost(message, tag = "6")]
        Rejected(super::DeleteRejected),
    }
}

/// A `DeleteRejected`: path and reason.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DeleteRejected {
    /// The refused path.
    #[prost(string, tag = "1")]
    pub(crate) path: String,
    /// Why the delete was refused.
    #[prost(string, tag = "2")]
    pub(crate) reason: String,
}

/// A `GrepResult` carrying only the error arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GrepResult {
    /// The result; only error is ever sent.
    #[prost(oneof = "grep_result::Result", tags = "2")]
    pub(crate) result: Option<grep_result::Result>,
}

/// Nested arms of [`GrepResult`].
pub(crate) mod grep_result {
    /// The error arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// The grep failed without running.
        #[prost(message, tag = "2")]
        Error(super::GrepError),
    }
}

/// A `GrepError`: the failure text.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GrepError {
    /// The failure text.
    #[prost(string, tag = "1")]
    pub(crate) error: String,
}

/// A `ReadResult` carrying only the refusal arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ReadResult {
    /// The result; only refusal is ever sent.
    #[prost(oneof = "read_result::Result", tags = "3")]
    pub(crate) result: Option<read_result::Result>,
}

/// Nested arms of [`ReadResult`].
pub(crate) mod read_result {
    /// The refusal arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// The read was refused without running.
        #[prost(message, tag = "3")]
        Rejected(super::ReadRejected),
    }
}

/// A `ReadRejected`: path and reason.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ReadRejected {
    /// The refused path.
    #[prost(string, tag = "1")]
    pub(crate) path: String,
    /// Why the read was refused.
    #[prost(string, tag = "2")]
    pub(crate) reason: String,
}

/// An `LsResult` carrying only the refusal arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct LsResult {
    /// The result; only refusal is ever sent.
    #[prost(oneof = "ls_result::Result", tags = "3")]
    pub(crate) result: Option<ls_result::Result>,
}

/// Nested arms of [`LsResult`].
pub(crate) mod ls_result {
    /// The refusal arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// The listing was refused without running.
        #[prost(message, tag = "3")]
        Rejected(super::LsRejected),
    }
}

/// An `LsRejected`: path and reason.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct LsRejected {
    /// The refused path.
    #[prost(string, tag = "1")]
    pub(crate) path: String,
    /// Why the listing was refused.
    #[prost(string, tag = "2")]
    pub(crate) reason: String,
}

/// A `DiagnosticsResult` carrying only the refusal arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DiagnosticsResult {
    /// The result; only refusal is ever sent.
    #[prost(oneof = "diagnostics_result::Result", tags = "3")]
    pub(crate) result: Option<diagnostics_result::Result>,
}

/// Nested arms of [`DiagnosticsResult`].
pub(crate) mod diagnostics_result {
    /// The refusal arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// Diagnostics were refused without running.
        #[prost(message, tag = "3")]
        Rejected(super::DiagnosticsRejected),
    }
}

/// A `DiagnosticsRejected`: path and reason.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DiagnosticsRejected {
    /// The refused path.
    #[prost(string, tag = "1")]
    pub(crate) path: String,
    /// Why diagnostics were refused.
    #[prost(string, tag = "2")]
    pub(crate) reason: String,
}

/// A `FetchResult` carrying only the error arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct FetchResult {
    /// The result; only error is ever sent.
    #[prost(oneof = "fetch_result::Result", tags = "2")]
    pub(crate) result: Option<fetch_result::Result>,
}

/// Nested arms of [`FetchResult`].
pub(crate) mod fetch_result {
    /// The error arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// The fetch failed without running.
        #[prost(message, tag = "2")]
        Error(super::FetchError),
    }
}

/// A `FetchError`: URL and failure text.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct FetchError {
    /// The refused URL.
    #[prost(string, tag = "1")]
    pub(crate) url: String,
    /// The failure text.
    #[prost(string, tag = "2")]
    pub(crate) error: String,
}

/// A `ListMcpResourcesExecResult` carrying only the success arm with no
/// resources.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ListMcpResourcesExecResult {
    /// The result; only empty success is ever sent.
    #[prost(oneof = "list_mcp_resources_result::Result", tags = "1")]
    pub(crate) result: Option<list_mcp_resources_result::Result>,
}

/// Nested arms of [`ListMcpResourcesExecResult`].
pub(crate) mod list_mcp_resources_result {
    /// The success arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// No resources on this host.
        #[prost(message, tag = "1")]
        Success(super::ListMcpResourcesSuccess),
    }
}

/// A `ListMcpResourcesSuccess`: always empty in this client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ListMcpResourcesSuccess {
    /// No entries; this host holds no live MCP connections.
    #[prost(message, repeated, tag = "1")]
    pub(crate) resources: Vec<McpResource>,
}

/// An MCP resource entry; never populated in this client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpResource {
    /// The resource URI.
    #[prost(string, tag = "1")]
    pub(crate) uri: String,
    /// The server name.
    #[prost(string, tag = "5")]
    pub(crate) server: String,
}

/// A `ReadMcpResourceExecResult` carrying only the not-found arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ReadMcpResourceExecResult {
    /// The result; only not-found is ever sent.
    #[prost(oneof = "read_mcp_resource_result::Result", tags = "4")]
    pub(crate) result: Option<read_mcp_resource_result::Result>,
}

/// Nested arms of [`ReadMcpResourceExecResult`].
pub(crate) mod read_mcp_resource_result {
    /// The not-found arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// No such resource on this host.
        #[prost(message, tag = "4")]
        NotFound(super::ReadMcpResourceNotFound),
    }
}

/// A `ReadMcpResourceNotFound`: URI and reason.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ReadMcpResourceNotFound {
    /// The unknown URI.
    #[prost(string, tag = "1")]
    pub(crate) uri: String,
    /// Why the read failed.
    #[prost(string, tag = "2")]
    pub(crate) reason: String,
}

/// An `McpStateExecResult` carrying only the success arm with no servers.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpStateExecResult {
    /// The result; only empty success is ever sent.
    #[prost(oneof = "mcp_state_result::Result", tags = "1")]
    pub(crate) result: Option<mcp_state_result::Result>,
}

/// Nested arms of [`McpStateExecResult`].
pub(crate) mod mcp_state_result {
    /// The success arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// No servers on this host.
        #[prost(message, tag = "1")]
        Success(super::McpStateSuccess),
    }
}

/// An `McpStateSuccess`: always empty in this client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpStateSuccess {
    /// No entries; this host holds no live MCP connections.
    #[prost(message, repeated, tag = "1")]
    pub(crate) servers: Vec<McpStateServer>,
}

/// An MCP server entry; never populated in this client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpStateServer {}

/// A `ShellAllowlistPrecheckResult`: never allowlisted in this client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ShellAllowlistPrecheckResult {
    /// Always false.
    #[prost(bool, tag = "1")]
    pub(crate) allowlisted: bool,
}

/// An `McpAllowlistPrecheckResult`: never allowlisted in this client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpAllowlistPrecheckResult {
    /// Always false.
    #[prost(bool, tag = "1")]
    pub(crate) allowlisted: bool,
}

/// A `WebFetchAllowlistPrecheckResult`: never allowlisted in this client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct WebFetchAllowlistPrecheckResult {
    /// Always false.
    #[prost(bool, tag = "1")]
    pub(crate) allowlisted: bool,
}

/// One `ExecClientControlMessage`: throw, stream close, or heartbeat.
///
/// Oneof arms: stream close 1, throw 2, heartbeat 3.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExecClientControlMessage {
    /// The control message; absent is never sent.
    #[prost(oneof = "exec_client_control::Body", tags = "1, 2, 3")]
    pub(crate) message: Option<exec_client_control::Body>,
}

/// Nested arms of [`ExecClientControlMessage`].
pub(crate) mod exec_client_control {
    /// One control arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Body {
        /// Ends one exec stream.
        #[prost(message, tag = "1")]
        StreamClose(super::ExecClientStreamClose),
        /// Fails one exec frame in band.
        #[prost(message, tag = "2")]
        Throw(super::ExecClientThrow),
        /// The exec keepalive.
        #[prost(message, tag = "3")]
        Heartbeat(super::ExecClientHeartbeat),
    }
}

/// An `ExecClientStreamClose`: ends one exec stream.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExecClientStreamClose {
    /// The numeric exec id being closed.
    #[prost(uint32, tag = "1")]
    pub(crate) id: u32,
}

/// An `ExecClientThrow`: fails one exec frame in band.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExecClientThrow {
    /// The numeric exec id being failed.
    #[prost(uint32, tag = "1")]
    pub(crate) id: u32,
    /// The failure text the model sees.
    #[prost(string, tag = "2")]
    pub(crate) error: String,
    /// A machine-readable failure code.
    #[prost(string, optional, tag = "4")]
    pub(crate) error_code: Option<String>,
}

/// An `ExecClientHeartbeat`: the exec keepalive.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExecClientHeartbeat {
    /// The numeric exec id being kept alive.
    #[prost(uint32, tag = "1")]
    pub(crate) id: u32,
}

/// A `ClientHeartbeat`: the run keepalive.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ClientHeartbeat {}

/// One `ExecServerControlMessage`: an abort for one exec frame.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExecServerControlMessage {
    /// The control message; only abort exists.
    #[prost(oneof = "exec_server_control::Body", tags = "1")]
    pub(crate) message: Option<exec_server_control::Body>,
}

/// Nested arms of [`ExecServerControlMessage`].
pub(crate) mod exec_server_control {
    /// The abort arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Body {
        /// Aborts one exec frame by numeric id.
        #[prost(message, tag = "1")]
        Abort(super::ExecServerAbort),
    }
}

/// An `ExecServerAbort`: abort one exec frame by numeric id.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExecServerAbort {
    /// The numeric exec id to abort.
    #[prost(uint32, tag = "1")]
    pub(crate) id: u32,
}

/// One `KvServerMessage`: a blob-store get or set.
///
/// Oneof arms: get blob 2, set blob 3. Numeric id 1 routes the reply.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct KvServerMessage {
    /// The numeric KV id the reply echoes.
    #[prost(uint32, tag = "1")]
    pub(crate) id: u32,
    /// The request; only get and set exist.
    #[prost(oneof = "kv_server_message::Body", tags = "2, 3")]
    pub(crate) message: Option<kv_server_message::Body>,
}

/// Nested arms of [`KvServerMessage`].
pub(crate) mod kv_server_message {
    /// One KV request arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Body {
        /// Reads one blob by id.
        #[prost(message, tag = "2")]
        GetBlob(super::GetBlobArgs),
        /// Stores one blob under its id.
        #[prost(message, tag = "3")]
        SetBlob(super::SetBlobArgs),
    }
}

/// A `GetBlobArgs`: one blob id.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetBlobArgs {
    /// The blob id.
    #[prost(bytes, tag = "1")]
    pub(crate) blob_id: Vec<u8>,
}

/// A `SetBlobArgs`: one blob id plus bytes.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct SetBlobArgs {
    /// The blob id.
    #[prost(bytes, tag = "1")]
    pub(crate) blob_id: Vec<u8>,
    /// The blob bytes.
    #[prost(bytes, tag = "2")]
    pub(crate) blob_data: Vec<u8>,
}

/// One `KvClientMessage`: a blob answer.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct KvClientMessage {
    /// The numeric KV id being answered.
    #[prost(uint32, tag = "1")]
    pub(crate) id: u32,
    /// The answer; absent is never sent.
    #[prost(oneof = "kv_client_message::Body", tags = "2, 3")]
    pub(crate) message: Option<kv_client_message::Body>,
}

/// Nested arms of [`KvClientMessage`].
pub(crate) mod kv_client_message {
    /// One KV answer arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Body {
        /// The blob bytes, or nothing when unknown.
        #[prost(message, tag = "2")]
        GetBlobResult(super::GetBlobResult),
        /// The store acknowledgment.
        #[prost(message, tag = "3")]
        SetBlobResult(super::SetBlobResult),
    }
}

/// A `GetBlobResult`: the blob bytes when known.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetBlobResult {
    /// The bytes; absent answers unknown ids with nothing.
    #[prost(bytes, optional, tag = "1")]
    pub(crate) blob_data: Option<Vec<u8>>,
}

/// A `SetBlobResult`: always success in this client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct SetBlobResult {}

/// One `InteractionQuery`: a permission query the client must answer.
///
/// All eight arms are modeled as empty bodies: only the arm name matters,
/// since every query is refused without reading its payload. Numeric id 1
/// routes the reply.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct InteractionQuery {
    /// The numeric query id the reply echoes.
    #[prost(uint32, tag = "1")]
    pub(crate) id: u32,
    /// The query; absent queries end the run rather than strand the server.
    #[prost(oneof = "interaction_query::Query", tags = "2, 3, 4, 5, 6, 7, 8, 9")]
    pub(crate) query: Option<interaction_query::Query>,
}

/// Nested arms of [`InteractionQuery`].
pub(crate) mod interaction_query {
    /// One query arm; bodies are never read.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Query {
        /// A hosted web-search permission query; refused.
        #[prost(message, tag = "2")]
        WebSearch(super::EmptyQuery),
        /// An interactive question; refused.
        #[prost(message, tag = "3")]
        AskQuestion(super::EmptyQuery),
        /// A mode switch; refused.
        #[prost(message, tag = "4")]
        SwitchMode(super::EmptyQuery),
        /// A hosted Exa search; refused.
        #[prost(message, tag = "5")]
        ExaSearch(super::EmptyQuery),
        /// A hosted Exa fetch; refused.
        #[prost(message, tag = "6")]
        ExaFetch(super::EmptyQuery),
        /// A plan-file creation; refused.
        #[prost(message, tag = "7")]
        CreatePlan(super::EmptyQuery),
        /// A VM setup; unanswerable, ends the run.
        #[prost(message, tag = "8")]
        SetupVm(super::EmptyQuery),
        /// A hosted web fetch; refused.
        #[prost(message, tag = "9")]
        WebFetch(super::EmptyQuery),
    }
}

/// An empty query body: the arm name is the whole signal.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct EmptyQuery {}

/// One `InteractionResponse`: a permission answer.
///
/// Only the refusal arms this client sends are modeled: web search 2,
/// ask-question 3, switch mode 4, Exa search 5, Exa fetch 6, create plan 7,
/// web fetch 9. VM setup has no answer and ends the run instead.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct InteractionResponse {
    /// The numeric query id being answered.
    #[prost(uint32, tag = "1")]
    pub(crate) id: u32,
    /// The answer; absent is never sent.
    #[prost(oneof = "interaction_response::Result", tags = "2, 3, 4, 5, 6, 7, 9")]
    pub(crate) result: Option<interaction_response::Result>,
}

/// Nested arms of [`InteractionResponse`].
pub(crate) mod interaction_response {
    /// One answer arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// A web-search refusal.
        #[prost(message, tag = "2")]
        WebSearch(super::WebSearchRequestResponse),
        /// An ask-question refusal.
        #[prost(message, tag = "3")]
        AskQuestion(super::AskQuestionInteractionResponse),
        /// A mode-switch refusal.
        #[prost(message, tag = "4")]
        SwitchMode(super::SwitchModeRequestResponse),
        /// An Exa-search refusal.
        #[prost(message, tag = "5")]
        ExaSearch(super::ExaSearchRequestResponse),
        /// An Exa-fetch refusal.
        #[prost(message, tag = "6")]
        ExaFetch(super::ExaFetchRequestResponse),
        /// A create-plan error.
        #[prost(message, tag = "7")]
        CreatePlan(super::CreatePlanRequestResponse),
        /// A web-fetch refusal.
        #[prost(message, tag = "9")]
        WebFetch(super::WebFetchRequestResponse),
    }
}

/// A `WebSearchRequestResponse` carrying only the refusal arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct WebSearchRequestResponse {
    /// The result; only refusal is ever sent.
    #[prost(oneof = "web_search_response::Result", tags = "2")]
    pub(crate) result: Option<web_search_response::Result>,
}

/// Nested arms of [`WebSearchRequestResponse`].
pub(crate) mod web_search_response {
    /// The refusal arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// Hosted web search is refused without running.
        #[prost(message, tag = "2")]
        Rejected(super::QueryRejected),
    }
}

/// An `ExaSearchRequestResponse` carrying only the refusal arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExaSearchRequestResponse {
    /// The result; only refusal is ever sent.
    #[prost(oneof = "exa_search_response::Result", tags = "2")]
    pub(crate) result: Option<exa_search_response::Result>,
}

/// Nested arms of [`ExaSearchRequestResponse`].
pub(crate) mod exa_search_response {
    /// The refusal arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// Hosted Exa search is refused without running.
        #[prost(message, tag = "2")]
        Rejected(super::QueryRejected),
    }
}

/// An `ExaFetchRequestResponse` carrying only the refusal arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExaFetchRequestResponse {
    /// The result; only refusal is ever sent.
    #[prost(oneof = "exa_fetch_response::Result", tags = "2")]
    pub(crate) result: Option<exa_fetch_response::Result>,
}

/// Nested arms of [`ExaFetchRequestResponse`].
pub(crate) mod exa_fetch_response {
    /// The refusal arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// Hosted Exa fetch is refused without running.
        #[prost(message, tag = "2")]
        Rejected(super::QueryRejected),
    }
}

/// A `WebFetchRequestResponse` carrying only the refusal arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct WebFetchRequestResponse {
    /// The result; only refusal is ever sent.
    #[prost(oneof = "web_fetch_response::Result", tags = "2")]
    pub(crate) result: Option<web_fetch_response::Result>,
}

/// Nested arms of [`WebFetchRequestResponse`].
pub(crate) mod web_fetch_response {
    /// The refusal arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// Hosted web fetch is refused without running.
        #[prost(message, tag = "2")]
        Rejected(super::QueryRejected),
    }
}

/// A `SwitchModeRequestResponse` carrying only the refusal arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct SwitchModeRequestResponse {
    /// The result; only refusal is ever sent.
    #[prost(oneof = "switch_mode_response::Result", tags = "2")]
    pub(crate) result: Option<switch_mode_response::Result>,
}

/// Nested arms of [`SwitchModeRequestResponse`].
pub(crate) mod switch_mode_response {
    /// The refusal arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// Mode switches are refused.
        #[prost(message, tag = "2")]
        Rejected(super::QueryRejected),
    }
}

/// A `*_Rejected` body: one reason string at field 1, shared by the web,
/// Exa, fetch, and mode responses.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct QueryRejected {
    /// Why the query was refused.
    #[prost(string, tag = "1")]
    pub(crate) reason: String,
}

/// An `AskQuestionInteractionResponse`: one nested result.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AskQuestionInteractionResponse {
    /// The result.
    #[prost(message, tag = "1")]
    pub(crate) result: Option<AskQuestionResult>,
}

/// An `AskQuestionResult` carrying only the refusal arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AskQuestionResult {
    /// The result; only refusal is ever sent.
    #[prost(oneof = "ask_question_result::Result", tags = "3")]
    pub(crate) result: Option<ask_question_result::Result>,
}

/// Nested arms of [`AskQuestionResult`].
pub(crate) mod ask_question_result {
    /// The refusal arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// Interactive questions are refused.
        #[prost(message, tag = "3")]
        Rejected(super::AskQuestionRejected),
    }
}

/// An `AskQuestionRejected`: one reason string.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AskQuestionRejected {
    /// Why the question was refused.
    #[prost(string, tag = "1")]
    pub(crate) reason: String,
}

/// A `CreatePlanRequestResponse`: one nested result.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CreatePlanRequestResponse {
    /// The result.
    #[prost(message, tag = "1")]
    pub(crate) result: Option<CreatePlanResult>,
}

/// A `CreatePlanResult` carrying only the error arm.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CreatePlanResult {
    /// The result; only error is ever sent.
    #[prost(oneof = "create_plan_result::Result", tags = "2")]
    pub(crate) result: Option<create_plan_result::Result>,
}

/// Nested arms of [`CreatePlanResult`].
pub(crate) mod create_plan_result {
    /// The error arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// Plan files are not created by this client.
        #[prost(message, tag = "2")]
        Error(super::CreatePlanError),
    }
}

/// A `CreatePlanError`: one error string.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CreatePlanError {
    /// Why no plan file was created.
    #[prost(string, tag = "1")]
    pub(crate) error: String,
}

/// An `aiserver.v1.ErrorDetails`: the typed Cursor failure inside Connect
/// error details. Numeric code 1, details message 2.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ErrorDetails {
    /// The numeric Cursor error code.
    #[prost(int32, tag = "1")]
    pub(crate) error: i32,
    /// Human details with the retry flag.
    #[prost(message, tag = "2")]
    pub(crate) details: Option<CustomErrorDetails>,
}

/// An `aiserver.v1.CustomErrorDetails`: title, detail, and retry flag.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CustomErrorDetails {
    /// The short title.
    #[prost(string, tag = "1")]
    pub(crate) title: String,
    /// The longer detail.
    #[prost(string, tag = "2")]
    pub(crate) detail: String,
    /// Whether the failure is retryable.
    #[prost(bool, optional, tag = "4")]
    pub(crate) is_retryable: Option<bool>,
}

/// `CursorError` numeric codes used by the error mapping.
pub(crate) struct CursorError;

impl CursorError {
    /// Bad API key.
    pub(crate) const BAD_API_KEY: i32 = 1;
    /// Not logged in.
    pub(crate) const NOT_LOGGED_IN: i32 = 2;
    /// Invalid auth id.
    pub(crate) const INVALID_AUTH_ID: i32 = 3;
    /// Permissions too low.
    pub(crate) const NOT_HIGH_ENOUGH_PERMISSIONS: i32 = 4;
    /// Bad model name.
    pub(crate) const BAD_MODEL_NAME: i32 = 5;
    /// User not found.
    pub(crate) const USER_NOT_FOUND: i32 = 6;
    /// Free rate limit exceeded.
    pub(crate) const FREE_USER_RATE_LIMIT_EXCEEDED: i32 = 7;
    /// Pro rate limit exceeded.
    pub(crate) const PRO_USER_RATE_LIMIT_EXCEEDED: i32 = 8;
    /// Free usage limit reached.
    pub(crate) const FREE_USER_USAGE_LIMIT: i32 = 9;
    /// Pro usage limit reached.
    pub(crate) const PRO_USER_USAGE_LIMIT: i32 = 10;
    /// Auth token not found.
    pub(crate) const AUTH_TOKEN_NOT_FOUND: i32 = 11;
    /// Auth token expired.
    pub(crate) const AUTH_TOKEN_EXPIRED: i32 = 12;
    /// Upstream model provider failure.
    pub(crate) const OPENAI: i32 = 13;
    /// Upstream rate limit.
    pub(crate) const OPENAI_RATE_LIMIT_EXCEEDED: i32 = 14;
    /// Login required.
    pub(crate) const AGENT_REQUIRES_LOGIN: i32 = 18;
    /// Output token ceiling hit.
    pub(crate) const MAX_TOKENS: i32 = 20;
    /// The user aborted the request.
    pub(crate) const USER_ABORTED_REQUEST: i32 = 21;
    /// Generic rate limit.
    pub(crate) const GENERIC_RATE_LIMIT_EXCEEDED: i32 = 22;
    /// Pro-only feature.
    pub(crate) const PRO_USER_ONLY: i32 = 23;
    /// Upstream timeout.
    pub(crate) const TIMEOUT: i32 = 25;
    /// Vision-model rate limit.
    pub(crate) const GPT_4_VISION_PREVIEW_RATE_LIMIT: i32 = 28;
    /// Custom server message.
    pub(crate) const CUSTOM_MESSAGE: i32 = 29;
    /// Client build too old.
    pub(crate) const OUTDATED_CLIENT: i32 = 30;
    /// Image too large for the Claude route.
    pub(crate) const CLAUDE_IMAGE_TOO_LARGE: i32 = 31;
    /// File not found.
    pub(crate) const FILE_NOT_FOUND: i32 = 33;
    /// API-key rate limit.
    pub(crate) const API_KEY_RATE_LIMIT: i32 = 34;
    /// Request debounced by the server.
    pub(crate) const DEBOUNCED: i32 = 35;
    /// Bad request.
    pub(crate) const BAD_REQUEST: i32 = 36;
    /// Repository service not initialized.
    pub(crate) const REPOSITORY_SERVICE_REPOSITORY_IS_NOT_INITIALIZED: i32 = 37;
    /// Unauthorized.
    pub(crate) const UNAUTHORIZED: i32 = 38;
    /// Not found.
    pub(crate) const NOT_FOUND: i32 = 39;
    /// Deprecated endpoint.
    pub(crate) const DEPRECATED: i32 = 40;
    /// Resource exhausted.
    pub(crate) const RESOURCE_EXHAUSTED: i32 = 41;
    /// Bad user API key.
    pub(crate) const BAD_USER_API_KEY: i32 = 42;
    /// Conversation too long.
    pub(crate) const CONVERSATION_TOO_LONG: i32 = 43;
    /// Usage pricing required.
    pub(crate) const USAGE_PRICING_REQUIRED: i32 = 44;
    /// Usage pricing required, changeable.
    pub(crate) const USAGE_PRICING_REQUIRED_CHANGEABLE: i32 = 45;
    /// No GitHub credentials for the user.
    pub(crate) const GITHUB_NO_USER_CREDENTIALS: i32 = 46;
    /// The GitHub user lacks access.
    pub(crate) const GITHUB_USER_NO_ACCESS: i32 = 47;
    /// The GitHub app lacks access.
    pub(crate) const GITHUB_APP_NO_ACCESS: i32 = 48;
    /// Several GitHub owners match.
    pub(crate) const GITHUB_MULTIPLE_OWNERS: i32 = 49;
    /// GitHub rate limited.
    pub(crate) const RATE_LIMITED: i32 = 50;
    /// GitHub rate limited, changeable.
    pub(crate) const RATE_LIMITED_CHANGEABLE: i32 = 51;
    /// Custom server failure.
    pub(crate) const CUSTOM: i32 = 52;
    /// Blocked by a hook.
    pub(crate) const HOOKS_BLOCKED: i32 = 53;
    /// Blocked for suspicious usage.
    pub(crate) const SUSPICIOUS_USAGE_BLOCKED: i32 = 54;
    /// Extension host timeout.
    pub(crate) const EXTENSION_HOST_TIMEOUT: i32 = 55;
    /// Network error.
    pub(crate) const NETWORK_ERROR: i32 = 56;
    /// Provider error.
    pub(crate) const PROVIDER_ERROR: i32 = 57;
    /// Model blocked.
    pub(crate) const MODEL_BLOCKED: i32 = 58;
    /// Internal error.
    pub(crate) const INTERNAL: i32 = 59;
    /// Max mode required.
    pub(crate) const MAX_MODE_REQUIRED: i32 = 60;
    /// Model no longer supported.
    pub(crate) const MODEL_NO_LONGER_SUPPORTED: i32 = 61;
    /// Pricing warning.
    pub(crate) const PRICING_WARNING: i32 = 62;
    /// Slow pool.
    pub(crate) const SLOW_POOL: i32 = 63;
    /// Unsupported region.
    pub(crate) const UNSUPPORTED_REGION: i32 = 64;
    /// Account closed.
    pub(crate) const ACCOUNT_CLOSED: i32 = 65;

    /// Codes the reference treats as retryable without an explicit flag.
    pub(crate) const RETRYABLE: [i32; 7] = [
        Self::OPENAI,
        Self::TIMEOUT,
        Self::EXTENSION_HOST_TIMEOUT,
        Self::NETWORK_ERROR,
        Self::PROVIDER_ERROR,
        Self::INTERNAL,
        Self::SLOW_POOL,
    ];

    /// The display name of a numeric code, `ERROR_<n>` when unknown.
    #[must_use]
    pub(crate) fn name(code: i32) -> String {
        match code {
            Self::BAD_API_KEY => String::from("ERROR_BAD_API_KEY"),
            Self::NOT_LOGGED_IN => String::from("ERROR_NOT_LOGGED_IN"),
            Self::INVALID_AUTH_ID => String::from("ERROR_INVALID_AUTH_ID"),
            Self::NOT_HIGH_ENOUGH_PERMISSIONS => String::from("ERROR_NOT_HIGH_ENOUGH_PERMISSIONS"),
            Self::BAD_MODEL_NAME => String::from("ERROR_BAD_MODEL_NAME"),
            Self::USER_NOT_FOUND => String::from("ERROR_USER_NOT_FOUND"),
            Self::FREE_USER_RATE_LIMIT_EXCEEDED => {
                String::from("ERROR_FREE_USER_RATE_LIMIT_EXCEEDED")
            }
            Self::PRO_USER_RATE_LIMIT_EXCEEDED => {
                String::from("ERROR_PRO_USER_RATE_LIMIT_EXCEEDED")
            }
            Self::FREE_USER_USAGE_LIMIT => String::from("ERROR_FREE_USER_USAGE_LIMIT"),
            Self::PRO_USER_USAGE_LIMIT => String::from("ERROR_PRO_USER_USAGE_LIMIT"),
            Self::AUTH_TOKEN_NOT_FOUND => String::from("ERROR_AUTH_TOKEN_NOT_FOUND"),
            Self::AUTH_TOKEN_EXPIRED => String::from("ERROR_AUTH_TOKEN_EXPIRED"),
            Self::OPENAI => String::from("ERROR_OPENAI"),
            Self::OPENAI_RATE_LIMIT_EXCEEDED => String::from("ERROR_OPENAI_RATE_LIMIT_EXCEEDED"),
            Self::AGENT_REQUIRES_LOGIN => String::from("ERROR_AGENT_REQUIRES_LOGIN"),
            Self::MAX_TOKENS => String::from("ERROR_MAX_TOKENS"),
            Self::USER_ABORTED_REQUEST => String::from("ERROR_USER_ABORTED_REQUEST"),
            Self::GENERIC_RATE_LIMIT_EXCEEDED => String::from("ERROR_GENERIC_RATE_LIMIT_EXCEEDED"),
            Self::PRO_USER_ONLY => String::from("ERROR_PRO_USER_ONLY"),
            Self::TIMEOUT => String::from("ERROR_TIMEOUT"),
            Self::GPT_4_VISION_PREVIEW_RATE_LIMIT => {
                String::from("ERROR_GPT_4_VISION_PREVIEW_RATE_LIMIT")
            }
            Self::CUSTOM_MESSAGE => String::from("ERROR_CUSTOM_MESSAGE"),
            Self::OUTDATED_CLIENT => String::from("ERROR_OUTDATED_CLIENT"),
            Self::CLAUDE_IMAGE_TOO_LARGE => String::from("ERROR_CLAUDE_IMAGE_TOO_LARGE"),
            Self::FILE_NOT_FOUND => String::from("ERROR_FILE_NOT_FOUND"),
            Self::API_KEY_RATE_LIMIT => String::from("ERROR_API_KEY_RATE_LIMIT"),
            Self::DEBOUNCED => String::from("ERROR_DEBOUNCED"),
            Self::BAD_REQUEST => String::from("ERROR_BAD_REQUEST"),
            Self::REPOSITORY_SERVICE_REPOSITORY_IS_NOT_INITIALIZED => {
                String::from("ERROR_REPOSITORY_SERVICE_REPOSITORY_IS_NOT_INITIALIZED")
            }
            Self::UNAUTHORIZED => String::from("ERROR_UNAUTHORIZED"),
            Self::NOT_FOUND => String::from("ERROR_NOT_FOUND"),
            Self::DEPRECATED => String::from("ERROR_DEPRECATED"),
            Self::RESOURCE_EXHAUSTED => String::from("ERROR_RESOURCE_EXHAUSTED"),
            Self::BAD_USER_API_KEY => String::from("ERROR_BAD_USER_API_KEY"),
            Self::CONVERSATION_TOO_LONG => String::from("ERROR_CONVERSATION_TOO_LONG"),
            Self::USAGE_PRICING_REQUIRED => String::from("ERROR_USAGE_PRICING_REQUIRED"),
            Self::USAGE_PRICING_REQUIRED_CHANGEABLE => {
                String::from("ERROR_USAGE_PRICING_REQUIRED_CHANGEABLE")
            }
            Self::GITHUB_NO_USER_CREDENTIALS => String::from("ERROR_GITHUB_NO_USER_CREDENTIALS"),
            Self::GITHUB_USER_NO_ACCESS => String::from("ERROR_GITHUB_USER_NO_ACCESS"),
            Self::GITHUB_APP_NO_ACCESS => String::from("ERROR_GITHUB_APP_NO_ACCESS"),
            Self::GITHUB_MULTIPLE_OWNERS => String::from("ERROR_GITHUB_MULTIPLE_OWNERS"),
            Self::RATE_LIMITED => String::from("ERROR_RATE_LIMITED"),
            Self::RATE_LIMITED_CHANGEABLE => String::from("ERROR_RATE_LIMITED_CHANGEABLE"),
            Self::CUSTOM => String::from("ERROR_CUSTOM"),
            Self::HOOKS_BLOCKED => String::from("ERROR_HOOKS_BLOCKED"),
            Self::SUSPICIOUS_USAGE_BLOCKED => String::from("ERROR_SUSPICIOUS_USAGE_BLOCKED"),
            Self::EXTENSION_HOST_TIMEOUT => String::from("ERROR_EXTENSION_HOST_TIMEOUT"),
            Self::NETWORK_ERROR => String::from("ERROR_NETWORK_ERROR"),
            Self::PROVIDER_ERROR => String::from("ERROR_PROVIDER_ERROR"),
            Self::MODEL_BLOCKED => String::from("ERROR_MODEL_BLOCKED"),
            Self::INTERNAL => String::from("ERROR_INTERNAL"),
            Self::MAX_MODE_REQUIRED => String::from("ERROR_MAX_MODE_REQUIRED"),
            Self::MODEL_NO_LONGER_SUPPORTED => String::from("ERROR_MODEL_NO_LONGER_SUPPORTED"),
            Self::PRICING_WARNING => String::from("ERROR_PRICING_WARNING"),
            Self::SLOW_POOL => String::from("ERROR_SLOW_POOL"),
            Self::UNSUPPORTED_REGION => String::from("ERROR_UNSUPPORTED_REGION"),
            Self::ACCOUNT_CLOSED => String::from("ERROR_ACCOUNT_CLOSED"),
            _ => format!("ERROR_{code}"),
        }
    }
}

/// One `ConversationTurnStructure`: a user turn plus its steps, stored as
/// blob ids. Only the agent arm is modeled; shell turns decode into
/// nothing.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ConversationTurnStructure {
    /// The turn; only agent turns are modeled.
    #[prost(oneof = "conversation_turn::Turn", tags = "1")]
    pub(crate) turn: Option<conversation_turn::Turn>,
}

/// Nested arms of [`ConversationTurnStructure`].
pub(crate) mod conversation_turn {
    /// The agent-turn arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Turn {
        /// A user turn with steps.
        #[prost(message, tag = "1")]
        AgentConversationTurn(super::AgentConversationTurnStructure),
    }
}

/// One `AgentConversationTurnStructure`: the user-message blob id plus the
/// step blob ids.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AgentConversationTurnStructure {
    /// The `UserMessage` blob id.
    #[prost(bytes, tag = "1")]
    pub(crate) user_message: Vec<u8>,
    /// The `ConversationStep` blob ids in order.
    #[prost(bytes, repeated, tag = "2")]
    pub(crate) steps: Vec<Vec<u8>>,
}

/// One `ConversationStep`: an assistant message, a tool call, or thinking.
///
/// Oneof arms: assistant message 1, tool call 2, thinking message 3.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ConversationStep {
    /// The step; absent is never sent.
    #[prost(oneof = "conversation_step::Step", tags = "1, 2, 3")]
    pub(crate) message: Option<conversation_step::Step>,
}

/// Nested arms of [`ConversationStep`].
pub(crate) mod conversation_step {
    /// One step arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Step {
        /// Assistant text.
        #[prost(message, tag = "1")]
        AssistantMessage(super::StepAssistantMessage),
        /// A tool call with its history result.
        #[prost(message, tag = "2")]
        ToolCall(super::HistoryToolCall),
        /// Thinking text.
        #[prost(message, tag = "3")]
        ThinkingMessage(super::StepThinkingMessage),
    }
}

/// An assistant-message step: text only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct StepAssistantMessage {
    /// The text.
    #[prost(string, tag = "1")]
    pub(crate) text: String,
}

/// A thinking-message step: text only.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct StepThinkingMessage {
    /// The thinking text.
    #[prost(string, tag = "1")]
    pub(crate) text: String,
}

/// A history `ToolCall`: the envelope id plus the typed MCP call with its
/// recorded result. Only the MCP arm is modeled.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct HistoryToolCall {
    /// The call; `None` for every non-MCP tool.
    #[prost(oneof = "history_tool_call::Tool", tags = "15")]
    pub(crate) tool: Option<history_tool_call::Tool>,
    /// The call id carried on the envelope.
    #[prost(string, optional, tag = "57")]
    pub(crate) tool_call_id: Option<String>,
}

/// Nested arms of [`HistoryToolCall`].
pub(crate) mod history_tool_call {
    /// The MCP arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Tool {
        /// A recorded dal tool call.
        #[prost(message, tag = "15")]
        McpToolCall(super::McpToolCallHistory),
    }
}

/// A recorded MCP tool call: arguments plus the recorded result.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpToolCallHistory {
    /// The call arguments.
    #[prost(message, tag = "1")]
    pub(crate) args: Option<McpArgs>,
    /// The recorded result, when the call settled.
    #[prost(message, tag = "2")]
    pub(crate) result: Option<McpToolResultHistory>,
}

/// A recorded MCP tool result: success, error, refusal, or denial.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpToolResultHistory {
    /// The result; absent is never sent.
    #[prost(oneof = "mcp_tool_result_history::Result", tags = "1, 2, 3, 4")]
    pub(crate) result: Option<mcp_tool_result_history::Result>,
}

/// Nested arms of [`McpToolResultHistory`].
pub(crate) mod mcp_tool_result_history {
    /// One recorded result arm.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Result {
        /// The tool ran.
        #[prost(message, tag = "1")]
        Success(super::McpToolSuccessHistory),
        /// The tool ran and failed.
        #[prost(message, tag = "2")]
        Error(super::McpToolErrorHistory),
        /// The call was refused without running.
        #[prost(message, tag = "3")]
        Rejected(super::McpToolRejectedHistory),
        /// The tool denied the call.
        #[prost(message, tag = "4")]
        PermissionDenied(super::McpToolPermissionDeniedHistory),
    }
}

/// A recorded MCP success: content plus the error flag.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpToolSuccessHistory {
    /// Text and image items in order.
    #[prost(message, repeated, tag = "1")]
    pub(crate) content: Vec<McpContentItem>,
    /// True when the content reports a tool error.
    #[prost(bool, tag = "2")]
    pub(crate) is_error: bool,
}

/// A recorded MCP error: the failure text.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpToolErrorHistory {
    /// The failure text.
    #[prost(string, tag = "1")]
    pub(crate) error: String,
}

/// A recorded MCP refusal: the reason.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpToolRejectedHistory {
    /// Why the call was refused.
    #[prost(string, tag = "1")]
    pub(crate) reason: String,
}

/// A recorded MCP denial: the failure text.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct McpToolPermissionDeniedHistory {
    /// The failure text.
    #[prost(string, tag = "1")]
    pub(crate) error: String,
}
