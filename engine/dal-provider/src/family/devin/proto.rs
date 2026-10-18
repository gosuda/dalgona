//! Devin (Codeium Cascade) protobuf wire types, `prost` derive-only.
//!
//! Every struct here is a hand-written transcription of the matching message
//! in the pinned reference's vendored descriptors (`a4dc3302`,
//! `packages/ai/src/providers/devin/proto/exa/`), verified field by field
//! against `api_server.proto`, `chat.proto`, `auth.proto`,
//! `codeium_common.proto`, and `seat_management.proto`. Field numbers and
//! scalar types are exact; `prost` skips unknown fields on decode, so the
//! structs carry only the fields this family sends or reads, and the omissions
//! are documented on each struct. No `protoc`, no `prost-build`: the
//! `prost 0.14.4` dependency exists for its derive macros only.
//!
//! Enums stay plain `i32` fields with `pub(crate)` constant modules beside
//! them. That keeps the unknown-variant surface honest: the server may send a
//! discriminant this transcription never saw, and an `i32` round-trips it
//! instead of failing the whole message.

/// `google.protobuf.Timestamp`: seconds since the Unix epoch plus nanos.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct Timestamp {
    /// Seconds since 1970-01-01T00:00:00Z.
    #[prost(int64, tag = "1")]
    pub(crate) seconds: i64,
    /// Nanoseconds within the second.
    #[prost(int32, tag = "2")]
    pub(crate) nanos: i32,
}

/// `exa.codeium_common_pb.Metadata`: request identity and auth material.
///
/// Auth rides inside this message (`api_key`, then `user_jwt` once the
/// session token has been exchanged); Devin sends no HTTP auth header.
/// Only the identity fields this family sets are carried; the rest keep
/// proto3 defaults.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct Metadata {
    /// CLI product name, always `"devin-cli"` on chat calls.
    #[prost(string, tag = "1")]
    pub(crate) ide_name: String,
    /// CLI version of the pinned reference identity.
    #[prost(string, tag = "7")]
    pub(crate) ide_version: String,
    /// Client kind; `"chisel"` unlocks assignment and the CLI model surface.
    #[prost(string, tag = "28")]
    pub(crate) ide_type: String,
    /// Extension name, always `"chisel"`.
    #[prost(string, tag = "12")]
    pub(crate) extension_name: String,
    /// Extension version, mirroring the CLI version.
    #[prost(string, tag = "2")]
    pub(crate) extension_version: String,
    /// Session token (scheme-prefixed) or the raw key on the 401 fallback.
    #[prost(string, tag = "3")]
    pub(crate) api_key: String,
    /// Locale, always `"en"`.
    #[prost(string, tag = "4")]
    pub(crate) locale: String,
    /// OS vocabulary: `darwin`, `windows`, or `linux`.
    #[prost(string, tag = "5")]
    pub(crate) os: String,
    /// Hardware description; unset by this family.
    #[prost(string, tag = "8")]
    pub(crate) hardware: String,
    /// Telemetry opt-out; unset by this family.
    #[prost(bool, tag = "6")]
    pub(crate) disable_telemetry: bool,
    /// Session id; unset by this family.
    #[prost(string, tag = "10")]
    pub(crate) session_id: String,
    /// Language-server timestamp; unset by this family.
    #[prost(message, optional, tag = "16")]
    pub(crate) ls_timestamp: Option<Timestamp>,
    /// Request id; unset by this family.
    #[prost(uint64, tag = "9")]
    pub(crate) request_id: u64,
    /// Source address; unset by this family.
    #[prost(string, tag = "11")]
    pub(crate) source_address: String,
    /// User agent; unset by this family.
    #[prost(string, tag = "13")]
    pub(crate) user_agent: String,
    /// Request URL; unset by this family.
    #[prost(string, tag = "14")]
    pub(crate) url: String,
    /// Auth source; unset by this family.
    #[prost(int32, tag = "15")]
    pub(crate) auth_source: i32,
    /// Extension path; unset by this family.
    #[prost(string, tag = "17")]
    pub(crate) extension_path: String,
    /// User id; unset by this family.
    #[prost(string, tag = "20")]
    pub(crate) user_id: String,
    /// JWT from `GetUserJwt`; empty on the auth call itself.
    #[prost(string, tag = "21")]
    pub(crate) user_jwt: String,
    /// Forced team id; unset by this family.
    #[prost(string, tag = "22")]
    pub(crate) force_team_id: String,
    /// Device fingerprint; unset by this family.
    #[prost(string, tag = "24")]
    pub(crate) device_fingerprint: String,
    /// Trigger id; unset by this family.
    #[prost(string, tag = "25")]
    pub(crate) trigger_id: String,
    /// Plan name; unset by this family.
    #[prost(string, tag = "26")]
    pub(crate) plan_name: String,
    /// Request id alias; unset by this family.
    #[prost(string, tag = "27")]
    pub(crate) id: String,
    /// Impersonated tier; unset by this family.
    #[prost(string, tag = "29")]
    pub(crate) impersonate_tier: String,
    /// Display slots the client opts into; set on discovery calls only.
    #[prost(int32, repeated, tag = "30")]
    pub(crate) supported_model_displays: Vec<i32>,
    /// Opaque client field; unset by this family.
    #[prost(string, tag = "31")]
    pub(crate) f: String,
    /// Team id; unset by this family.
    #[prost(string, tag = "32")]
    pub(crate) team_id: String,
}

/// `exa.chat_pb.ChatMessagePrompt`: one history turn on the wire.
///
/// Omitted members (`num_tokens`, telemetry flags, redaction flags, annotation
/// ranges, output/thinking ids, Gemini thought bytes, phase) are never set by
/// the pinned reference's history builder and are skipped on decode.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ChatMessagePrompt {
    /// Stable per-turn id; see `history::deterministic_id`.
    #[prost(string, tag = "1")]
    pub(crate) message_id: String,
    /// Channel: `USER`, `SYSTEM` (assistant), or `TOOL` (result).
    #[prost(int32, tag = "2")]
    pub(crate) source: i32,
    /// Prompt text; assistant text plus demoted foreign thinking.
    #[prost(string, tag = "3")]
    pub(crate) prompt: String,
    /// Assistant tool calls of this turn.
    #[prost(message, repeated, tag = "6")]
    pub(crate) tool_calls: Vec<ChatToolCall>,
    /// Id of the call this result answers; set on `TOOL` prompts only.
    #[prost(string, tag = "7")]
    pub(crate) tool_call_id: String,
    /// Whether the result is an error; set on `TOOL` prompts only.
    #[prost(bool, tag = "9")]
    pub(crate) tool_result_is_error: bool,
    /// Inline images of the turn.
    #[prost(message, repeated, tag = "10")]
    pub(crate) images: Vec<ImageData>,
    /// Native thinking text; empty for foreign turns.
    #[prost(string, tag = "11")]
    pub(crate) thinking: String,
    /// Native thinking signature; empty for foreign turns.
    #[prost(string, tag = "12")]
    pub(crate) signature: String,
    /// Signature type; the pinned reference always sends empty.
    #[prost(string, tag = "18")]
    pub(crate) signature_type: String,
}

/// `exa.codeium_common_pb.ChatToolCall`: one streamed or replayed call.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ChatToolCall {
    /// Provider call id.
    #[prost(string, tag = "1")]
    pub(crate) id: String,
    /// Tool name.
    #[prost(string, tag = "2")]
    pub(crate) name: String,
    /// Raw arguments JSON, cumulative or incremental per delta.
    #[prost(string, tag = "3")]
    pub(crate) arguments_json: String,
}

/// `exa.chat_pb.ChatToolDefinition`: one advertised tool.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ChatToolDefinition {
    /// Wire tool name.
    #[prost(string, tag = "1")]
    pub(crate) name: String,
    /// Tool description.
    #[prost(string, tag = "2")]
    pub(crate) description: String,
    /// JSON Schema text, sent verbatim (Gemini routes excepted).
    #[prost(string, tag = "3")]
    pub(crate) json_schema_string: String,
    /// Strict mode; the pinned reference always sends false.
    #[prost(bool, tag = "12")]
    pub(crate) strict: bool,
}

/// `exa.chat_pb.ChatToolChoice`: the `choice` oneof as two optional members.
///
/// Wire-identical to the oneof while exactly one member is set, which is this
/// family's only use (`option_name: "auto"`).
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ChatToolChoice {
    /// Named choice, e.g. `"auto"`.
    #[prost(string, optional, tag = "1")]
    pub(crate) option_name: Option<String>,
    /// A single forced tool name; never set by this family.
    #[prost(string, optional, tag = "2")]
    pub(crate) tool_name: Option<String>,
}

/// `exa.chat_pb.PromptCacheOptions`: system-prompt cache control.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct PromptCacheOptions {
    /// `CacheControlType`; the pinned reference sends `EPHEMERAL`.
    #[prost(int32, tag = "1")]
    pub(crate) r#type: i32,
}

/// `exa.codeium_common_pb.CompletionConfiguration`: sampling and limits.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct CompletionConfiguration {
    /// Completion count; always 1.
    #[prost(uint64, tag = "1")]
    pub(crate) num_completions: u64,
    /// Output token ceiling.
    #[prost(uint64, tag = "2")]
    pub(crate) max_tokens: u64,
    /// Newline ceiling; the pinned reference sends 400.
    #[prost(uint64, tag = "3")]
    pub(crate) max_newlines: u64,
    /// Sampling temperature.
    #[prost(double, tag = "5")]
    pub(crate) temperature: f64,
    /// First-token temperature, mirroring `temperature`.
    #[prost(double, tag = "6")]
    pub(crate) first_temperature: f64,
    /// Top-k; the pinned reference sends 40.
    #[prost(uint64, tag = "7")]
    pub(crate) top_k: u64,
    /// Top-p as a float32 widened to double.
    #[prost(double, tag = "8")]
    pub(crate) top_p: f64,
    /// Caller stop sequences; empty unless the caller passes some.
    #[prost(string, repeated, tag = "9")]
    pub(crate) stop_patterns: Vec<String>,
    /// End-of-text probability threshold; the pinned reference sends 1.
    #[prost(double, tag = "11")]
    pub(crate) fim_eot_prob_threshold: f64,
}

/// `exa.codeium_common_pb.ModelUsageStats`: per-response token counters.
///
/// Omitted members (deprecated model enum, provider, message id, response
/// headers) carry no counters this family reads.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ModelUsageStats {
    /// Input tokens.
    #[prost(uint64, tag = "2")]
    pub(crate) input_tokens: u64,
    /// Output tokens.
    #[prost(uint64, tag = "3")]
    pub(crate) output_tokens: u64,
    /// Input tokens written to cache.
    #[prost(uint64, tag = "4")]
    pub(crate) cache_write_tokens: u64,
    /// Input tokens served from cache.
    #[prost(uint64, tag = "5")]
    pub(crate) cache_read_tokens: u64,
    /// Concrete model uid that served the turn; authoritative over assignment.
    #[prost(string, tag = "9")]
    pub(crate) model_uid: String,
    /// Billing model uid; unread by this family.
    #[prost(string, tag = "10")]
    pub(crate) billing_model_uid: String,
    /// Requested model uid; unread by this family.
    #[prost(string, tag = "11")]
    pub(crate) requested_model_uid: String,
}

/// `exa.codeium_common_pb.ImageData`: one inline image.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ImageData {
    /// Base64-encoded image bytes.
    #[prost(string, tag = "1")]
    pub(crate) base64_data: String,
    /// MIME type, e.g. `image/png`.
    #[prost(string, tag = "2")]
    pub(crate) mime_type: String,
    /// Caption; unset by this family.
    #[prost(string, tag = "3")]
    pub(crate) caption: String,
}

/// `exa.api_server_pb.GetChatMessageRequest`: one Cascade turn.
///
/// Omitted members (internal chat model, experiment config, chat model name,
/// trajectory reference, prompt id, provider source, language, arena fields)
/// are never set by the pinned reference's chat builder.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct GetChatMessageRequest {
    /// Identity and auth.
    #[prost(message, optional, tag = "1")]
    pub(crate) metadata: Option<Metadata>,
    /// Flattened system prompt.
    #[prost(string, tag = "2")]
    pub(crate) prompt: String,
    /// Conversation history, oldest first.
    #[prost(message, repeated, tag = "3")]
    pub(crate) chat_message_prompts: Vec<ChatMessagePrompt>,
    /// Resolved chat model uid; never the router uid.
    #[prost(string, tag = "21")]
    pub(crate) chat_model_uid: String,
    /// Request type; always `CASCADE`.
    #[prost(int32, tag = "7")]
    pub(crate) request_type: i32,
    /// Sampling and limits.
    #[prost(message, optional, tag = "8")]
    pub(crate) configuration: Option<CompletionConfiguration>,
    /// Advertised tools.
    #[prost(message, repeated, tag = "10")]
    pub(crate) tools: Vec<ChatToolDefinition>,
    /// True unless the model supports parallel tool calls.
    #[prost(bool, tag = "11")]
    pub(crate) disable_parallel_tool_calls: bool,
    /// Tool choice; always `option_name: "auto"`.
    #[prost(message, optional, tag = "12")]
    pub(crate) tool_choice: Option<ChatToolChoice>,
    /// System-prompt cache control; always `EPHEMERAL`.
    #[prost(message, optional, tag = "13")]
    pub(crate) system_prompt_cache_options: Option<PromptCacheOptions>,
    /// Cascade thread id, shared with the assignment call.
    #[prost(string, tag = "16")]
    pub(crate) cascade_id: String,
    /// Planner mode; always `DEFAULT`.
    #[prost(int32, tag = "20")]
    pub(crate) planner_mode: i32,
    /// Per-turn execution id.
    #[prost(string, tag = "22")]
    pub(crate) execution_id: String,
    /// Assignment JWT; present on router-resolved turns only.
    #[prost(string, optional, tag = "26")]
    pub(crate) model_assignment_jwt: Option<String>,
}

/// `exa.api_server_pb.GetChatMessageResponse`: one streamed delta.
///
/// Omitted members (timestamp, token counts, redact flags, latency, profiles,
/// output/thinking/request ids, prompt echo, Gemini thought bytes, phase,
/// quota basis points, dimension groups) carry nothing this family reads.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct GetChatMessageResponse {
    /// Response id; the first non-empty one wins.
    #[prost(string, tag = "1")]
    pub(crate) message_id: String,
    /// Text delta of this message.
    #[prost(string, tag = "3")]
    pub(crate) delta_text: String,
    /// Delta token count; unread by this family.
    #[prost(uint32, tag = "4")]
    pub(crate) delta_tokens: u32,
    /// Stop reason; `UNSPECIFIED` (0) means "not terminal yet".
    #[prost(int32, tag = "5")]
    pub(crate) stop_reason: i32,
    /// Tool-call deltas of this message.
    #[prost(message, repeated, tag = "6")]
    pub(crate) delta_tool_calls: Vec<ChatToolCall>,
    /// Token counters of the turn so far.
    #[prost(message, optional, tag = "7")]
    pub(crate) usage: Option<ModelUsageStats>,
    /// Thinking delta of this message.
    #[prost(string, tag = "9")]
    pub(crate) delta_thinking: String,
    /// Thinking signature for this delta; kept verbatim for replay.
    #[prost(string, tag = "10")]
    pub(crate) delta_signature: String,
    /// Credit cost; surfaced for accounting, never as USD.
    #[prost(int32, tag = "14")]
    pub(crate) credit_cost: i32,
    /// Committed credit cost.
    #[prost(int32, tag = "18")]
    pub(crate) committed_credit_cost: i32,
    /// Committed ACU cost.
    #[prost(double, tag = "22")]
    pub(crate) committed_acu_cost: f64,
    /// Concrete serving model; overrides the assignment uid when present.
    #[prost(string, optional, tag = "23")]
    pub(crate) actual_model_uid: Option<String>,
}

/// `exa.api_server_pb.AssignModelRequest`: router resolution.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct AssignModelRequest {
    /// Identity; the resolved wire key, no JWT yet.
    #[prost(message, optional, tag = "1")]
    pub(crate) metadata: Option<Metadata>,
    /// Router uid, e.g. `adaptive`.
    #[prost(string, tag = "2")]
    pub(crate) model_router_uid: String,
    /// Cascade thread id; must match the chat call or the JWT is rejected.
    #[prost(string, tag = "3")]
    pub(crate) cascade_id: String,
    /// The current user turn alone, with an empty message id.
    #[prost(message, optional, tag = "5")]
    pub(crate) chat_message_prompt: Option<ChatMessagePrompt>,
}

/// `exa.api_server_pb.ModelAssignment`: the router's answer.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ModelAssignment {
    /// JWT authorizing the resolved uid on the chat call.
    #[prost(string, tag = "1")]
    pub(crate) assignment_jwt: String,
    /// Concrete chat model uid.
    #[prost(string, tag = "2")]
    pub(crate) model_uid: String,
    /// Harness uids; unread by this family.
    #[prost(string, repeated, tag = "3")]
    pub(crate) harness_uids: Vec<String>,
}

/// `exa.api_server_pb.AssignModelResponse`: wraps the assignment.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct AssignModelResponse {
    /// The assignment; both members are required to proceed.
    #[prost(message, optional, tag = "1")]
    pub(crate) assignment: Option<ModelAssignment>,
}

/// `exa.auth_pb.GetUserJwtRequest`: session-token exchange.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct GetUserJwtRequest {
    /// Identity with the session token and an empty JWT.
    #[prost(message, optional, tag = "1")]
    pub(crate) metadata: Option<Metadata>,
}

/// `exa.auth_pb.GetUserJwtResponse`: JWT plus an optional base override.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct GetUserJwtResponse {
    /// User JWT for chat and assignment calls.
    #[prost(string, tag = "1")]
    pub(crate) user_jwt: String,
    /// Server-directed base URL override; empty means "keep the default".
    #[prost(string, tag = "2")]
    pub(crate) custom_api_server_url: String,
}

/// `exa.api_server_pb.GetCliModelConfigsRequest`: discovery.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct GetCliModelConfigsRequest {
    /// Dev-channel identity.
    #[prost(message, optional, tag = "1")]
    pub(crate) metadata: Option<Metadata>,
}

/// `exa.codeium_common_pb.DefaultOverrideModelConfig`: account default.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct DefaultOverrideModelConfig {
    /// Config version id; unread by this family.
    #[prost(string, tag = "2")]
    pub(crate) version_id: String,
    /// Default wire uid for the account.
    #[prost(string, tag = "3")]
    pub(crate) model_uid: String,
}

/// `exa.api_server_pb.GetCliModelConfigsResponse`: the model catalog.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct GetCliModelConfigsResponse {
    /// Server model configs in server order.
    #[prost(message, repeated, tag = "1")]
    pub(crate) client_model_configs: Vec<ClientModelConfig>,
    /// Account default override, when the server names one.
    #[prost(message, optional, tag = "3")]
    pub(crate) default_override_model_config: Option<DefaultOverrideModelConfig>,
}

/// `exa.codeium_common_pb.ClientModelConfig`: one server model row.
///
/// Omitted members (aliases, pricing type, legacy/beta flags, provider enums,
/// rollout fields, promo/fast status, cost tier, smart-friend uid, sort
/// keys) feed catalog presentation the join owns, not the wire codec.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ClientModelConfig {
    /// Display label.
    #[prost(string, tag = "1")]
    pub(crate) label: String,
    /// Wire uid; the catalog key.
    #[prost(string, tag = "22")]
    pub(crate) model_uid: String,
    /// Credit multiplier; unread by this family.
    #[prost(float, tag = "3")]
    pub(crate) credit_multiplier: f32,
    /// True when the server disabled the row.
    #[prost(bool, tag = "4")]
    pub(crate) disabled: bool,
    /// Image support flag, authoritative only with `model_info`.
    #[prost(bool, tag = "5")]
    pub(crate) supports_images: bool,
    /// Server provider enum; unread by this family.
    #[prost(int32, tag = "10")]
    pub(crate) provider: i32,
    /// Context window tokens; 0 means unknown.
    #[prost(int32, tag = "18")]
    pub(crate) max_tokens: i32,
    /// Rich model metadata, when the server sends it.
    #[prost(message, optional, tag = "23")]
    pub(crate) model_info: Option<ModelInfo>,
    /// Server description; sparse.
    #[prost(string, optional, tag = "27")]
    pub(crate) description: Option<String>,
    /// Smart-friend uid; unread by this family.
    #[prost(string, optional, tag = "29")]
    pub(crate) smart_friend_model_uid: Option<String>,
    /// Family lane metadata for the effort ladder.
    #[prost(message, optional, tag = "30")]
    pub(crate) model_family_metadata: Option<ModelFamilyMetadata>,
    /// Family default flag.
    #[prost(bool, tag = "31")]
    pub(crate) is_default_model_in_family: bool,
    /// Cost dimensions and rate cards.
    #[prost(message, repeated, tag = "32")]
    pub(crate) model_dimensions: Vec<ModelDimension>,
    /// Disable reason; unread by this family.
    #[prost(message, optional, tag = "33")]
    pub(crate) disabled_reason: Option<ModelDisabledReason>,
}

/// `exa.codeium_common_pb.ModelInfo`: rich per-model metadata.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ModelInfo {
    /// Context window tokens.
    #[prost(int32, tag = "4")]
    pub(crate) max_tokens: i32,
    /// Capability flags.
    #[prost(message, optional, tag = "6")]
    pub(crate) model_features: Option<ModelFeatures>,
    /// Output token ceiling; 0 means unknown.
    #[prost(int32, tag = "13")]
    pub(crate) max_output_tokens: i32,
    /// Harness uids; non-empty means a composite, never a router.
    #[prost(string, repeated, tag = "20")]
    pub(crate) harness_uids: Vec<String>,
    /// Display slot; decides router and visibility handling.
    #[prost(int32, tag = "22")]
    pub(crate) display_option: i32,
    /// Router marker.
    #[prost(bool, tag = "25")]
    pub(crate) is_model_router: bool,
}

/// `exa.codeium_common_pb.ModelFeatures`: capability flags.
#[derive(Clone, PartialEq, ::prost::Message)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors exa.codeium_common_pb.ModelFeatures proto wire flags"
)]
pub(crate) struct ModelFeatures {
    /// Image input support.
    #[prost(bool, tag = "11")]
    pub(crate) supports_images: bool,
    /// Tool-call support.
    #[prost(bool, tag = "12")]
    pub(crate) supports_tool_calls: bool,
    /// Parallel tool-call support.
    #[prost(bool, tag = "21")]
    pub(crate) supports_parallel_tool_calls: bool,
    /// Thinking support.
    #[prost(bool, tag = "15")]
    pub(crate) supports_thinking: bool,
}

/// `exa.codeium_common_pb.ModelFamilyMetadata`: the effort ladder of a lane.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ModelFamilyMetadata {
    /// Family display label, e.g. `"GPT-5.6 Sol"`.
    #[prost(string, tag = "1")]
    pub(crate) model_family_label: String,
    /// Axis entries: effort, thinking, fast mode, 1M context.
    #[prost(message, repeated, tag = "2")]
    pub(crate) entries: Vec<ModelFamilyMetadataEntry>,
    /// Family default flag.
    #[prost(bool, tag = "3")]
    pub(crate) is_default_model_in_family: bool,
}

/// One family axis entry.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ModelFamilyMetadataEntry {
    /// Axis key, e.g. `"effort"` or `"Reasoning Effort"`.
    #[prost(string, tag = "1")]
    pub(crate) key: String,
    /// Axis value.
    #[prost(message, optional, tag = "2")]
    pub(crate) value: Option<ModelFamilyMetadataValue>,
}

/// One family axis value.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ModelFamilyMetadataValue {
    /// Lane order; 1 selects the fast / 1M / thinking lane.
    #[prost(int32, tag = "1")]
    pub(crate) order: i32,
    /// Display name, e.g. `"High"`.
    #[prost(string, tag = "2")]
    pub(crate) name: String,
}

/// `exa.codeium_common_pb.ModelDimension`: one cost rate entry.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ModelDimension {
    /// Rate label: `input`, `cached input`, `output`, or a marker.
    #[prost(string, tag = "1")]
    pub(crate) label: String,
    /// Rate value in `denominator` units.
    #[prost(float, tag = "2")]
    pub(crate) value: f32,
    /// Rate denominator, e.g. `"1M tokens"`.
    #[prost(string, tag = "3")]
    pub(crate) denominator: String,
    /// Dimension kind: `COST` or `COST_FUZZY`.
    #[prost(int32, tag = "6")]
    pub(crate) kind: i32,
}

/// `exa.codeium_common_pb.ModelDisabledReason`: why a row is disabled.
#[derive(Clone, PartialEq, ::prost::Message)]
pub(crate) struct ModelDisabledReason {
    /// Short reason; unread by this family.
    #[prost(string, tag = "1")]
    pub(crate) short_reason: String,
}

/// `exa.codeium_common_pb.StopReason` wire values.
pub(crate) mod stop_reason {
    /// No terminal signal yet.
    pub(crate) const UNSPECIFIED: i32 = 0;
    /// Generation did not finish cleanly.
    pub(crate) const INCOMPLETE: i32 = 1;
    /// A stop pattern fired; the normal text end.
    pub(crate) const STOP_PATTERN: i32 = 2;
    /// The output token limit was reached.
    pub(crate) const MAX_TOKENS: i32 = 3;
    /// Minimum log-probability stop.
    pub(crate) const MIN_LOG_PROB: i32 = 4;
    /// Newline ceiling stop.
    pub(crate) const MAX_NEWLINES: i32 = 5;
    /// Scope-exit stop.
    pub(crate) const EXIT_SCOPE: i32 = 6;
    /// Non-finite logit stop.
    pub(crate) const NONFINITE_LOGIT_OR_PROB: i32 = 7;
    /// First-non-whitespace-line stop.
    pub(crate) const FIRST_NON_WHITESPACE_LINE: i32 = 8;
    /// Partial generation.
    pub(crate) const PARTIAL: i32 = 9;
    /// The model requested tool calls.
    pub(crate) const FUNCTION_CALL: i32 = 10;
    /// A content filter stopped output.
    pub(crate) const CONTENT_FILTER: i32 = 11;
    /// Non-insertion stop.
    pub(crate) const NON_INSERTION: i32 = 12;
    /// Server-side error stop.
    pub(crate) const ERROR: i32 = 13;

    /// The canonical proto name of a discriminant, for `StopReason::Other`.
    #[must_use]
    pub(crate) const fn name(value: i32) -> &'static str {
        match value {
            UNSPECIFIED => "STOP_REASON_UNSPECIFIED",
            INCOMPLETE => "STOP_REASON_INCOMPLETE",
            STOP_PATTERN => "STOP_REASON_STOP_PATTERN",
            MAX_TOKENS => "STOP_REASON_MAX_TOKENS",
            MIN_LOG_PROB => "STOP_REASON_MIN_LOG_PROB",
            MAX_NEWLINES => "STOP_REASON_MAX_NEWLINES",
            EXIT_SCOPE => "STOP_REASON_EXIT_SCOPE",
            NONFINITE_LOGIT_OR_PROB => "STOP_REASON_NONFINITE_LOGIT_OR_PROB",
            FIRST_NON_WHITESPACE_LINE => "STOP_REASON_FIRST_NON_WHITESPACE_LINE",
            PARTIAL => "STOP_REASON_PARTIAL",
            FUNCTION_CALL => "STOP_REASON_FUNCTION_CALL",
            CONTENT_FILTER => "STOP_REASON_CONTENT_FILTER",
            NON_INSERTION => "STOP_REASON_NON_INSERTION",
            ERROR => "STOP_REASON_ERROR",
            _ => "STOP_REASON_UNKNOWN",
        }
    }
}

/// `exa.codeium_common_pb.ChatMessageSource` wire values.
///
/// Only the channels this family sends are named; any other discriminant
/// still decodes as its `i32`.
pub(crate) mod chat_message_source {
    /// User or developer turn.
    pub(crate) const USER: i32 = 1;
    /// Assistant turn.
    pub(crate) const SYSTEM: i32 = 2;
    /// Tool result turn.
    pub(crate) const TOOL: i32 = 4;
}

/// `exa.api_server_pb.ChatMessageRequestType` wire values.
///
/// Only the request type this family sends is named; any other discriminant
/// still decodes as its `i32`.
pub(crate) mod chat_message_request_type {
    /// Cascade chat; this family's only request type.
    pub(crate) const CASCADE: i32 = 5;
}

/// `exa.codeium_common_pb.ConversationalPlannerMode` wire values.
///
/// Only the planner mode this family sends is named; any other discriminant
/// still decodes as its `i32`.
pub(crate) mod conversational_planner_mode {
    /// Default planning; this family's only planner mode.
    pub(crate) const DEFAULT: i32 = 1;
}

/// `exa.chat_pb.CacheControlType` wire values.
///
/// Only the cache control this family sends is named; any other discriminant
/// still decodes as its `i32`.
pub(crate) mod cache_control_type {
    /// Ephemeral cache breakpoint; this family's only cache control.
    pub(crate) const EPHEMERAL: i32 = 1;
}

/// `exa.codeium_common_pb.ModelDimensionKind` wire values.
///
/// Only the billed-rate kinds are named; any other discriminant still
/// decodes as its `i32` and is skipped by cost normalization.
pub(crate) mod model_dimension_kind {
    /// A billed rate.
    pub(crate) const COST: i32 = 1;
    /// An estimated rate; read like `COST`.
    pub(crate) const COST_FUZZY: i32 = 2;
}

/// `exa.codeium_common_pb.DisplayOption` wire values.
///
/// The vendored descriptor predates slots 6-8, so these travel as plain
/// `i32`; the extra values decode and re-encode faithfully. Only the slots
/// this family advertises or filters on are named.
pub(crate) mod display_option {
    /// Router slot; marks `AssignModel` dispatchers.
    pub(crate) const MODEL_ROUTER: i32 = 3;
    /// Quick-review slot; requested but never surfaced.
    pub(crate) const QUICK_REVIEW: i32 = 4;
    /// Internal default slot; requested but never surfaced.
    pub(crate) const INTERNAL_DEFAULT: i32 = 6;
    /// Unclassified native slot; requested for parity.
    pub(crate) const UNCLASSIFIED: i32 = 7;
    /// Second visible-model slot for normal models.
    pub(crate) const NORMAL: i32 = 8;
}
