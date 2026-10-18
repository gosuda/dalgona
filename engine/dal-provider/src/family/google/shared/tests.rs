//! Witnesses for the shared `GenerateContent` grammar: byte-exact bodies,
//! thinking mappings, chunk decoding, terminal rules, replay carriers, and
//! HTTP error classification. Every stream case feeds captured-shaped wire
//! bytes through [`GeminiDecoder`]; nothing here echoes a mock.

use dal_core::{
    AssistantPart, CallId, ContextItem, ModelRoute, ModelToolSpec, Part, Purpose, ReplaySource,
    RequestParams, ThinkingLevel,
};
use futures::{StreamExt, executor::block_on};

use super::*;
use crate::{
    retry::RequestState,
    sse::SseEvent,
    thinking::{ThinkingPlan, WireThinking},
};

fn raw(text: &str) -> RawJson {
    RawJson::parse(text).unwrap()
}

fn source() -> ReplaySource {
    ReplaySource {
        family: Family::GoogleGenerativeAi,
        model: "gemini-3-pro".into(),
    }
}

fn plan(effort: Option<&'static str>) -> ThinkingPlan {
    ThinkingPlan {
        level: ThinkingLevel::High,
        wire: WireThinking::OpenAi { effort },
        temperature: None,
        notices: Vec::new(),
    }
}

fn request(context: Vec<ContextItem>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::GoogleGenerativeAi,
            model: "gemini-3-pro".into(),
        },
        system: "You are terse.".into(),
        tools: Vec::new().into(),
        context: context.into(),
        params: RequestParams {
            thinking: ThinkingLevel::High,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

fn compat() -> Compat {
    Compat {
        part_ids: true,
        skip: SignaturePolicy::Always,
        nested_tool_images: true,
        legacy_parameters: false,
        replay_thinking: true,
        tool_mode: ToolMode::Auto,
    }
}

fn google_request<'a>(request: &'a ModelRequest, thinking: &'a ThinkingPlan) -> GoogleRequest<'a> {
    GoogleRequest {
        family: Family::GoogleGenerativeAi,
        request,
        thinking,
        compat: compat(),
        image_input: true,
        dialect: Dialect::Flat { safety_off: false },
    }
}

fn text(value: &str) -> Part {
    Part::Text { text: value.into() }
}

fn decode(data: &[&str]) -> Vec<StreamEvent> {
    let mut decoder =
        GeminiDecoder::new(Family::GoogleGenerativeAi, "google", "gemini-3-pro", None);
    let mut out = Vec::new();
    for chunk in data {
        let progress = decoder.feed(chunk, &mut out).unwrap();
        assert_eq!(progress, Progress::Open);
    }
    decoder.end(&mut out).unwrap();
    out
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[test]
fn minimal_flat_body_is_byte_exact() {
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let thinking = plan(Some("high"));
    let body = request_body(&google_request(&request, &thinking)).unwrap();
    assert_eq!(
        String::from_utf8(body).unwrap(),
        "{\"contents\":[{\"role\":\"user\",\"parts\":[{\"text\":\"Hi\"}]}],\"systemInstruction\":{\"parts\":[{\"text\":\"You are terse.\"}]},\"generationConfig\":{\"thinkingConfig\":{\"includeThoughts\":true,\"thinkingLevel\":\"HIGH\"}}}"
    );
}

#[test]
fn flat_body_carries_tools_images_and_merged_responses() {
    let request = ModelRequest {
        tools: vec![ModelToolSpec {
            name: "read".into(),
            description: "Read a file.".into(),
            parameters: raw(r#"{"type":"object"}"#),
            grammar: None,
            defer_loading: false,
        }]
        .into(),
        context: vec![
            ContextItem::Assistant {
                source: source(),
                replay: None,
                parts: vec![
                    AssistantPart::Thinking {
                        text: "hidden".into(),
                        replay: None,
                    },
                    AssistantPart::ToolCall {
                        call: CallId::new("call_1"),
                        name: "read".into(),
                        args: raw(r#"{"path":"a.txt"}"#),
                        replay: None,
                    },
                ],
            },
            ContextItem::ToolResult {
                call: CallId::new("call_1"),
                name: "read".into(),
                is_error: false,
                parts: vec![
                    text("line1"),
                    Part::Image {
                        mime: "image/png".into(),
                        bytes: b"\x89PNG".as_slice().into(),
                    },
                ],
            },
        ]
        .into(),
        ..request(vec![])
    };
    let thinking = plan(Some("high"));
    let body =
        String::from_utf8(request_body(&google_request(&request, &thinking)).unwrap()).unwrap();
    assert!(body.contains("\"role\":\"model\""), "{body}");
    assert!(
        body.contains("\"text\":\"hidden\",\"thought\":true"),
        "{body}"
    );
    assert!(
        body.contains(
            "\"functionCall\":{\"name\":\"read\",\"args\":{\"path\":\"a.txt\"},\"id\":\"call_1\"}"
        ),
        "{body}"
    );
    // Unsigned call on the Always policy carries the bypass sentinel.
    assert!(
        body.contains("\"thoughtSignature\":\"skip_thought_signature_validator\""),
        "{body}"
    );
    assert!(
        body.contains(
            "\"functionResponse\":{\"name\":\"read\",\"response\":{\"output\":\"line1\"}"
        ),
        "{body}"
    );
    // Gemini 3+ nests the tool image inside the response, with no trailing
    // image turn.
    assert!(body.contains("\"inlineData\":"), "{body}");
    // The Google projection gives a bare object schema its empty
    // `properties` (`ensureObjectProperties` in the reference).
    assert!(
        body.contains("\"parametersJsonSchema\":{\"type\":\"object\",\"properties\":{}}"),
        "{body}"
    );
}

#[test]
fn stored_signature_replays_only_when_bound() {
    let carrier = raw(r#"{"calls":{"call_1":"QUJDRA=="}}"#);
    let parts = [AssistantPart::ToolCall {
        call: CallId::new("call_1"),
        name: "read".into(),
        args: raw(r"{}"),
        replay: None,
    }];
    let thinking = [AssistantPart::Thinking {
        text: "prior".into(),
        replay: Some(carrier),
    }];
    let mut with_carrier = vec![thinking[0].clone()];
    with_carrier.extend(parts.iter().cloned());
    let bound_request = request(vec![ContextItem::Assistant {
        source: source(),
        replay: None,
        parts: with_carrier,
    }]);
    let thinking = plan(Some("high"));
    let body = String::from_utf8(request_body(&google_request(&bound_request, &thinking)).unwrap())
        .unwrap();
    assert!(body.contains("\"thoughtSignature\":\"QUJDRA==\""), "{body}");

    let foreign = ReplaySource {
        family: Family::GoogleGenerativeAi,
        model: "other-model".into(),
    };
    let foreign_request = request(vec![ContextItem::Assistant {
        source: foreign,
        replay: None,
        parts: vec![AssistantPart::ToolCall {
            call: CallId::new("call_1"),
            name: "read".into(),
            args: raw(r"{}"),
            replay: None,
        }],
    }]);
    let body =
        String::from_utf8(request_body(&google_request(&foreign_request, &thinking)).unwrap())
            .unwrap();
    assert!(!body.contains("QUJDRA=="), "{body}");
}

#[test]
fn cca_envelope_orders_members_and_labels() {
    let request = request(vec![ContextItem::User {
        parts: vec![text("Hi")],
    }]);
    let thinking = plan(Some("medium"));
    let labels = vec![(String::from("trajectory_id"), String::from("t1"))];
    let input = GoogleRequest {
        dialect: Dialect::CloudCodeAssist {
            project: "proj",
            session_id: "sess",
            request_id: "agent/a/1/t/2",
            user_agent: "antigravity",
            labels: &labels,
            max_output_tokens: Some(65535),
        },
        ..google_request(&request, &thinking)
    };
    let body = String::from_utf8(request_body(&input).unwrap()).unwrap();
    for member in [
        "\"project\":\"proj\"",
        "\"requestId\":\"agent/a/1/t/2\"",
        "\"model\":\"gemini-3-pro\"",
        "\"userAgent\":\"antigravity\"",
        "\"requestType\":\"agent\"",
        "\"sessionId\":\"sess\"",
        "\"labels\":{\"trajectory_id\":\"t1\"}",
        "\"maxOutputTokens\":65535",
        "\"thinkingLevel\":\"MEDIUM\"",
        "\"systemInstruction\":{\"role\":\"user\"",
    ] {
        assert!(body.contains(member), "{body}");
    }
    let project = body.find("\"project\"").unwrap();
    let request_id = body.find("\"requestId\"").unwrap();
    let model = body.find("\"model\":\"gemini-3-pro\"").unwrap();
    assert!(project < request_id && request_id < model);
}

#[test]
fn thinking_effort_maps_to_level_spellings() {
    let request = request(vec![]);
    let wire = |effort| {
        let thinking = ThinkingPlan {
            wire: WireThinking::OpenAi { effort },
            ..plan(None)
        };
        thinking_config(&google_request(&request, &thinking))
            .and_then(|config| config.thinking_level)
    };
    assert_eq!(wire(None), None);
    assert_eq!(wire(Some("none")), None);
    assert_eq!(wire(Some("minimal")), Some("MINIMAL"));
    assert_eq!(wire(Some("low")), Some("LOW"));
    assert_eq!(wire(Some("medium")), Some("MEDIUM"));
    assert_eq!(wire(Some("high")), Some("HIGH"));
    assert_eq!(wire(Some("xhigh")), Some("HIGH"));
    assert_eq!(wire(Some("max")), Some("HIGH"));
}

#[test]
fn budget_plans_map_to_thinking_budget() {
    let request = request(vec![]);
    let config = thinking_config(&GoogleRequest {
        thinking: &ThinkingPlan {
            wire: WireThinking::Anthropic {
                thinking: AnthropicThinking::Enabled {
                    budget_tokens: 2048,
                    max_tokens: 8192,
                },
                effort: None,
            },
            ..plan(None)
        },
        ..google_request(&request, &plan(None))
    })
    .unwrap();
    assert_eq!(config.thinking_budget, Some(2048));
    assert_eq!(config.thinking_level, None);
    assert!(config.include_thoughts);
}

#[test]
fn wrong_family_route_names_the_family() {
    let mut request = request(vec![]);
    request.model = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-6".into(),
    };
    let thinking = plan(Some("high"));
    let error = request_body(&google_request(&request, &thinking)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "a google-generative-ai request needs a google-generative-ai model route, not `gpt-6`"
    );
}

fn call_part(id: &str) -> AssistantPart {
    AssistantPart::ToolCall {
        call: CallId::new(id),
        name: "read".into(),
        args: raw(r"{}"),
        replay: None,
    }
}

fn result_for(id: &str) -> ContextItem {
    ContextItem::ToolResult {
        call: CallId::new(id),
        name: "read".into(),
        is_error: false,
        parts: vec![text("ok")],
    }
}

fn png() -> Part {
    Part::Image {
        mime: "image/png".into(),
        bytes: b"\x89PNG".as_slice().into(),
    }
}

fn body_text(request: &ModelRequest) -> String {
    let thinking = plan(Some("high"));
    String::from_utf8(request_body(&google_request(request, &thinking)).unwrap()).unwrap()
}

/// The `id` of every `functionCall` part and every `functionResponse` part of
/// a flat body, each in wire order.
fn wire_ids(body: &str) -> (Vec<String>, Vec<String>) {
    use sonic_rs::{JsonContainerTrait, JsonValueTrait};
    let root: sonic_rs::Value = sonic_rs::from_str(body).unwrap();
    let (mut calls, mut results) = (Vec::new(), Vec::new());
    for content in root["contents"].as_array().unwrap() {
        for part in content["parts"].as_array().unwrap() {
            for (member, sink) in [
                ("functionCall", &mut calls),
                ("functionResponse", &mut results),
            ] {
                if let Some(id) = part[member]["id"].as_str() {
                    sink.push(id.to_owned());
                }
            }
        }
    }
    (calls, results)
}

#[test]
fn call_ids_sanitize_to_one_alphabet_on_both_sides() {
    // A cross-provider id with `:` and `.` must reach the wire the same way
    // on the functionCall and on its functionResponse, or the pairing breaks.
    let request = request(vec![
        ContextItem::Assistant {
            source: source(),
            replay: None,
            parts: vec![call_part("toolu_01:abc.def")],
        },
        result_for("toolu_01:abc.def"),
    ]);
    let body = body_text(&request);
    assert!(
        body.contains(r#""functionCall":{"name":"read","args":{},"id":"toolu_01_abc_def"}"#),
        "{body}"
    );
    assert!(
        body.contains(
            r#""functionResponse":{"name":"read","response":{"output":"ok"},"id":"toolu_01_abc_def"}"#
        ),
        "{body}"
    );
}

#[test]
fn colliding_call_ids_stay_unique_and_paired() {
    // `a:b` and `a/b` both sanitize to `a_b`; a 70-character id and its
    // 64-character prefix both cap to the same 64 characters. Distinct raw
    // ids must keep distinct wire ids, and each result must pair with its call.
    let long = "x".repeat(70);
    let exact = "x".repeat(64);
    let request = request(vec![
        ContextItem::Assistant {
            source: source(),
            replay: None,
            parts: vec![
                call_part("a:b"),
                call_part("a/b"),
                call_part(&long),
                call_part(&exact),
            ],
        },
        result_for("a:b"),
        result_for("a/b"),
        result_for(&long),
        result_for(&exact),
    ]);
    let (calls, results) = wire_ids(&body_text(&request));
    assert_eq!(calls, results, "every result pairs with its call");
    assert_eq!(calls[0], "a_b");
    assert_eq!(calls[1], "a_b_dup1");
    assert_eq!(calls[2], exact);
    assert_eq!(calls[3], format!("{}_dup1", "x".repeat(59)));
    let distinct: std::collections::BTreeSet<_> = calls.iter().collect();
    assert_eq!(distinct.len(), calls.len(), "{calls:?}");
    assert!(
        calls.iter().all(|id| id.len() <= 64
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))),
        "{calls:?}"
    );
}

const PROJECTED_SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","description":"{minLength: 1}"}},"required":["path"]}"#;

fn schema_request() -> ModelRequest {
    ModelRequest {
        tools: vec![ModelToolSpec {
            name: "read".into(),
            description: "Read a file.".into(),
            parameters: raw(
                r#"{"$schema":"http://json-schema.org/draft-07/schema#","type":"object","properties":{"path":{"type":"string","minLength":1}},"required":["path"],"additionalProperties":false}"#,
            ),
            grammar: None,
            defer_loading: false,
        }]
        .into(),
        ..request(vec![ContextItem::User {
            parts: vec![text("Hi")],
        }])
    }
}

#[test]
fn modern_tool_schemas_are_projected_for_google() {
    let body = body_text(&schema_request());
    assert!(
        body.contains(&format!("\"parametersJsonSchema\":{PROJECTED_SCHEMA}")),
        "{body}"
    );
    assert!(!body.contains("$schema"), "{body}");
    assert!(!body.contains("additionalProperties"), "{body}");
}

#[test]
fn legacy_tool_schemas_are_projected_for_cca() {
    let request = schema_request();
    let thinking = plan(Some("high"));
    let input = GoogleRequest {
        compat: Compat {
            legacy_parameters: true,
            ..compat()
        },
        ..google_request(&request, &thinking)
    };
    let body = String::from_utf8(request_body(&input).unwrap()).unwrap();
    assert!(
        body.contains(&format!("\"parameters\":{PROJECTED_SCHEMA}")),
        "{body}"
    );
    assert!(!body.contains("parametersJsonSchema"), "{body}");
    assert!(!body.contains("$schema"), "{body}");
}

#[test]
fn non_vision_models_get_one_placeholder_in_place_of_images() {
    let request = request(vec![ContextItem::User {
        parts: vec![text("look"), png(), png()],
    }]);
    let thinking = plan(Some("high"));
    let input = GoogleRequest {
        image_input: false,
        ..google_request(&request, &thinking)
    };
    let body = String::from_utf8(request_body(&input).unwrap()).unwrap();
    assert!(!body.contains("inlineData"), "{body}");
    assert!(
        body.contains(
            r#""parts":[{"text":"look"},{"text":"[image omitted: model does not support vision]"}]"#
        ),
        "{body}"
    );
    // An image-only user turn degrades to the placeholder alone, not to an
    // empty (dropped) turn.
    let only = ModelRequest {
        context: vec![ContextItem::User { parts: vec![png()] }].into(),
        ..request
    };
    let input = GoogleRequest {
        image_input: false,
        ..google_request(&only, &thinking)
    };
    let body = String::from_utf8(request_body(&input).unwrap()).unwrap();
    assert!(
        body.contains(
            r#""contents":[{"role":"user","parts":[{"text":"[image omitted: model does not support vision]"}]}]"#
        ),
        "{body}"
    );
}

#[test]
fn non_vision_tool_results_name_the_omitted_image() {
    let request = request(vec![
        ContextItem::Assistant {
            source: source(),
            replay: None,
            parts: vec![call_part("call_1")],
        },
        ContextItem::ToolResult {
            call: CallId::new("call_1"),
            name: "read".into(),
            is_error: false,
            parts: vec![text("line1"), png()],
        },
    ]);
    let thinking = plan(Some("high"));
    let input = GoogleRequest {
        image_input: false,
        ..google_request(&request, &thinking)
    };
    let body = String::from_utf8(request_body(&input).unwrap()).unwrap();
    assert!(!body.contains("inlineData"), "{body}");
    assert!(!body.contains("Tool result image:"), "{body}");
    assert!(
        body.contains(
            r#""response":{"output":"line1\n[image omitted: model does not support vision]"}"#
        ),
        "{body}"
    );
}

#[test]
fn generation_parsing_gates_gemini_three() {
    assert_eq!(generation_of("gemini-3-pro").0, Some(3));
    assert_eq!(generation_of("gemini-2.5-flash").0, Some(2));
    assert_eq!(generation_of("gemini-flash-latest").0, None);
    assert_eq!(generation_of("claude-sonnet-4-6").0, None);
    assert!(generation_of("gemini-3-pro").is_three_or_later());
    assert!(!generation_of("gemini-2.5-flash").is_three_or_later());
    assert!(!is_claude_model("gemini-3-pro"));
    assert!(is_claude_model("claude-sonnet-4-6"));
}

// ---------------------------------------------------------------------------
// Stream decoding
// ---------------------------------------------------------------------------

/// Feeds every chunk then ends the input; returns the events emitted so far
/// and the outcome of `end`.
fn fed(chunks: &[&str]) -> (Vec<StreamEvent>, Result<(), ProviderError>) {
    let mut decoder =
        GeminiDecoder::new(Family::GoogleGenerativeAi, "google", "gemini-3-pro", None);
    let mut out = Vec::new();
    for chunk in chunks {
        decoder.feed(chunk, &mut out).unwrap();
    }
    let end = decoder.end(&mut out);
    (out, end)
}

#[test]
fn empty_stop_is_a_transport_error_before_any_terminal_event() {
    // A clean STOP with no text, no thinking, and no call is the empty
    // response failure: the decoder errors before any Usage/Stop so the
    // lifecycle's pre-delivery retry re-requests, instead of a silent
    // successful `Stop(EndTurn)`.
    for chunk in [
        r#"{"candidates":[{"content":{"parts":[{"text":""}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"totalTokenCount":3}}"#,
        r#"{"candidates":[{"finishReason":"STOP"}]}"#,
        // Whitespace-only text is the other face of the same failure.
        r#"{"candidates":[{"content":{"parts":[{"text":"\n "}]},"finishReason":"STOP"}]}"#,
    ] {
        let (out, end) = fed(&[chunk]);
        let error = end.unwrap_err();
        assert!(
            matches!(
                &error,
                ProviderError::Transport {
                    family: Family::GoogleGenerativeAi,
                    reason,
                } if reason.contains("empty response")
            ),
            "{error:?}"
        );
        assert!(
            out.is_empty(),
            "nothing was emitted before the error: {out:?}"
        );
    }
}

#[test]
fn thought_only_stop_names_the_thought_only_failure() {
    let (out, end) = fed(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"hmm","thought":true}]},"finishReason":"STOP"}]}"#,
    ]);
    let error = end.unwrap_err();
    assert!(
        matches!(
            &error,
            ProviderError::Transport { reason, .. } if reason.contains("thought-only")
        ),
        "{error:?}"
    );
    // The reasoning already streamed; no terminal group follows the error.
    assert_eq!(
        out,
        vec![StreamEvent::ReasoningDelta {
            text: String::from("hmm"),
        }]
    );
}

#[test]
fn empty_non_stop_finishes_keep_their_terminals() {
    // Only a clean STOP is the empty-response failure.
    let (out, end) = fed(&[r#"{"candidates":[{"finishReason":"MAX_TOKENS"}]}"#]);
    end.unwrap();
    assert!(matches!(
        out.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::MaxTokens,
        })
    ));
    let (out, end) = fed(&[r#"{"candidates":[{"finishReason":"SAFETY"}]}"#]);
    end.unwrap();
    assert!(matches!(
        out.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::Refusal,
        })
    ));
    // Held whitespace is released ahead of the terminal group.
    let (out, end) = fed(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"\n"}]},"finishReason":"MAX_TOKENS"}]}"#,
    ]);
    end.unwrap();
    assert_eq!(
        out[0],
        StreamEvent::TextDelta {
            text: String::from("\n"),
        }
    );
    assert!(matches!(
        out.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::MaxTokens,
        })
    ));
}

#[test]
fn leading_whitespace_keeps_its_place_before_the_answer() {
    // Whitespace held back while the response might still be empty comes out
    // in order once real text follows: the event sequence is unchanged.
    let (out, end) = fed(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"\n"}]}}]}"#,
        r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}]},"finishReason":"STOP"}]}"#,
    ]);
    end.unwrap();
    assert_eq!(
        out[..2],
        [
            StreamEvent::TextDelta {
                text: String::from("\n"),
            },
            StreamEvent::TextDelta {
                text: String::from("Hi"),
            },
        ]
    );
}

fn in_band(frame: &str) -> ProviderError {
    let mut decoder = GeminiDecoder::new(Family::GoogleAntigravity, "g", "gemini-3-pro", None);
    decoder.feed(frame, &mut Vec::new()).unwrap_err()
}

#[test]
fn in_band_daily_quota_frames_are_terminal_quota() {
    // Wording the reference classifies as account exhaustion.
    let wording = in_band(
        r#"{"error":{"code":429,"message":"You have exhausted your capacity on this model. Your quota will reset after 3h15m38s.","status":"RESOURCE_EXHAUSTED"}}"#,
    );
    assert!(
        matches!(wording, ProviderError::Quota { .. }),
        "{wording:?}"
    );
    // The `google.rpc.ErrorInfo` reason, inside the CCA envelope.
    let info = in_band(
        r#"{"response":{"error":{"code":429,"message":"Resource has been exhausted","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"QUOTA_EXHAUSTED"}]}}}"#,
    );
    assert!(matches!(info, ProviderError::Quota { .. }), "{info:?}");
    // A retry hint of five minutes or more.
    let long = in_band(
        r#"{"error":{"code":429,"message":"Resource has been exhausted","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"3600s"}]}}"#,
    );
    assert!(matches!(long, ProviderError::Quota { .. }), "{long:?}");
    // No numeric code: the RPC status alone maps to 429 and the same rule.
    let status_only = in_band(
        r#"{"error":{"message":"Your quota will reset after 1h.","status":"RESOURCE_EXHAUSTED"}}"#,
    );
    assert!(
        matches!(status_only, ProviderError::Quota { .. }),
        "{status_only:?}"
    );
}

#[test]
fn in_band_quota_details_only_upgrade_throttles() {
    // The details alone, with no wording and no numeric code, make the
    // exhaustion terminal.
    let credits = in_band(
        r#"{"error":{"message":"Resource has been exhausted","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"INSUFFICIENT_G1_CREDITS_BALANCE"}]}}"#,
    );
    assert!(
        matches!(credits, ProviderError::Quota { .. }),
        "{credits:?}"
    );
    // A frame that is not a throttle keeps the error the table gives it.
    let outage = in_band(
        r#"{"error":{"code":503,"message":"Service unavailable","status":"UNAVAILABLE","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"QUOTA_EXHAUSTED"}]}}"#,
    );
    assert!(!matches!(outage, ProviderError::Quota { .. }), "{outage:?}");
}

#[test]
fn in_band_short_retry_hints_stay_rate_limited() {
    let short = in_band(
        r#"{"error":{"code":429,"message":"Resource has been exhausted","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"30s"}]}}"#,
    );
    assert!(
        matches!(short, ProviderError::RateLimited { .. }),
        "{short:?}"
    );
}

#[test]
fn blocked_prompt_carries_reason_and_message_as_refusal_text() {
    let mut decoder = GeminiDecoder::new(Family::GoogleGenerativeAi, "g", "gemini-3-pro", None);
    let mut out = Vec::new();
    let progress = decoder
        .feed(
            r#"{"promptFeedback":{"blockReason":"SAFETY","blockReasonMessage":"Prompt was flagged."}}"#,
            &mut out,
        )
        .unwrap();
    assert_eq!(progress, Progress::Ended);
    assert_eq!(
        out[0],
        StreamEvent::TextDelta {
            text: String::from("Request blocked by Google (SAFETY): Prompt was flagged."),
        }
    );
    assert!(matches!(
        out.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::Refusal,
        })
    ));
    // Without a message the reason alone names the block.
    let mut decoder = GeminiDecoder::new(Family::GoogleGenerativeAi, "g", "gemini-3-pro", None);
    let mut out = Vec::new();
    decoder
        .feed(r#"{"promptFeedback":{"blockReason":"OTHER"}}"#, &mut out)
        .unwrap();
    assert_eq!(
        out[0],
        StreamEvent::TextDelta {
            text: String::from("Request blocked by Google (OTHER)"),
        }
    );
}

#[test]
fn text_stream_decodes_deltas_usage_and_stop() {
    let events = decode(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"Hello"}]}}]}"#,
        r#"{"candidates":[{"content":{"parts":[{"text":" world","thoughtSignature":"QUJD"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"thoughtsTokenCount":2,"totalTokenCount":17}}"#,
    ]);
    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta {
                text: String::from("Hello"),
            },
            StreamEvent::TextDelta {
                text: String::from(" world"),
            },
            // The signature on the second part signs the whole text block.
            StreamEvent::Replay {
                payload: ReplayPayload {
                    family: Family::GoogleGenerativeAi,
                    model: "gemini-3-pro".into(),
                    item: raw(
                        r#"{"parts":[{"kind":"text","index":0,"bytes":11,"signature":"QUJD"}]}"#
                    ),
                },
            },
            StreamEvent::ToolCallsDone { calls: Vec::new() },
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    output_tokens: 5,
                    reasoning_tokens: Some(2),
                    cache_write_tokens: 0,
                    cost_usd: None,
                    premium_requests: None,
                },
            },
            StreamEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]
    );
}

#[test]
fn thought_parts_become_reasoning_deltas() {
    let events = decode(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"let me think","thought":true},{"text":"done"}]},"finishReason":"STOP"}]}"#,
    ]);
    assert!(matches!(
        &events[0],
        StreamEvent::ReasoningDelta { text } if text == "let me think"
    ));
    assert!(matches!(
        &events[1],
        StreamEvent::TextDelta { text } if text == "done"
    ));
}

#[test]
fn function_call_assembles_with_signature_carrier() {
    let events = decode(&[
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{"path":"a.txt"},"id":"call_1"},"thoughtSignature":"QUJDRA=="}]},"finishReason":"STOP"}]}"#,
    ]);
    assert_eq!(
        events[0],
        StreamEvent::ToolCallStarted {
            id: String::from("call_1"),
            name: String::from("read"),
        }
    );
    assert_eq!(
        events[1],
        StreamEvent::ToolArgsDelta {
            id: String::from("call_1"),
            fragment: br#"{"path":"a.txt"}"#.to_vec(),
        }
    );
    let StreamEvent::Replay { payload } = &events[2] else {
        panic!("expected a replay carrier, got {:?}", events[2]);
    };
    assert_eq!(payload.family, Family::GoogleGenerativeAi);
    assert_eq!(payload.model.as_ref(), "gemini-3-pro");
    assert_eq!(payload.item.as_str(), r#"{"calls":{"call_1":"QUJDRA=="}}"#);
    let StreamEvent::ToolCallsDone { calls } = &events[3] else {
        panic!("expected tool calls, got {:?}", events[3]);
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, ToolArgs::from_bytes(br#"{"path":"a.txt"}"#));
    assert!(matches!(
        &events[5],
        StreamEvent::Stop {
            reason: StopReason::ToolUse,
        }
    ));
}

#[test]
fn decoded_signatures_replay_into_the_next_request() {
    // The decoder's carrier parses back through carrier_of: a signature that
    // survives a turn must arrive on the next turn's functionCall part.
    let mut decoder =
        GeminiDecoder::new(Family::GoogleGenerativeAi, "google", "gemini-3-pro", None);
    let mut out = Vec::new();
    decoder
        .feed(
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{},"id":"call_9"},"thoughtSignature":"QUJDRA=="}]},"finishReason":"STOP"}]}"#,
            &mut out,
        )
        .unwrap();
    decoder.end(&mut out).unwrap();
    let replay = out.iter().find_map(|event| match event {
        StreamEvent::Replay { payload } => Some(payload.item.clone()),
        _ => None,
    });
    let carrier = replay.expect("a signed call emits a carrier");
    let request = request(vec![ContextItem::Assistant {
        source: source(),
        replay: None,
        parts: vec![
            AssistantPart::Thinking {
                text: String::new().into(),
                replay: Some(carrier),
            },
            AssistantPart::ToolCall {
                call: CallId::new("call_9"),
                name: "read".into(),
                args: raw(r"{}"),
                replay: None,
            },
        ],
    }]);
    let thinking = plan(Some("high"));
    let body =
        String::from_utf8(request_body(&google_request(&request, &thinking)).unwrap()).unwrap();
    assert!(body.contains("\"thoughtSignature\":\"QUJDRA==\""), "{body}");
}

const SIGNED_TEXT_RESPONSE: &str = r#"{"candidates":[{"content":{"parts":[{"text":"plan","thought":true,"thoughtSignature":"dGhvdWdodA=="},{"text":"answer"},{"text":"","thoughtSignature":"YW5zd2Vy"}]},"finishReason":"STOP"}]}"#;

/// What the session fold makes of a signed response: the thought, the answer,
/// then an empty thinking part that carries the replay event's item.
fn folded_signed_message(events: &[StreamEvent], answer: &str) -> Vec<AssistantPart> {
    let item = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Replay { payload } => Some(payload.item.clone()),
            _ => None,
        })
        .expect("a signed response emits a carrier");
    vec![
        AssistantPart::Thinking {
            text: "plan".into(),
            replay: None,
        },
        AssistantPart::Text {
            text: answer.into(),
            replay: None,
        },
        AssistantPart::Thinking {
            text: String::new().into(),
            replay: Some(item),
        },
    ]
}

#[test]
fn text_and_thought_signatures_replay_onto_their_blocks() {
    let events = decode(&[SIGNED_TEXT_RESPONSE]);
    let StreamEvent::Replay { payload } = &events[2] else {
        panic!("expected a replay carrier, got {:?}", events[2]);
    };
    // A trailing signature-only part signs the text block that is open.
    assert_eq!(
        payload.item.as_str(),
        r#"{"parts":[{"kind":"thought","index":0,"bytes":4,"signature":"dGhvdWdodA=="},{"kind":"text","index":0,"bytes":6,"signature":"YW5zd2Vy"}]}"#
    );
    let request = request(vec![ContextItem::Assistant {
        source: source(),
        replay: None,
        parts: folded_signed_message(&events, "answer"),
    }]);
    let body = body_text(&request);
    assert!(
        body.contains(r#"{"text":"plan","thought":true,"thoughtSignature":"dGhvdWdodA=="}"#),
        "{body}"
    );
    assert!(
        body.contains(r#"{"text":"answer","thoughtSignature":"YW5zd2Vy"}"#),
        "{body}"
    );
}

#[test]
fn signatures_do_not_replay_onto_a_changed_block_or_a_foreign_message() {
    let events = decode(&[SIGNED_TEXT_RESPONSE]);
    // An edited answer is no longer the block the signature covers.
    let edited = request(vec![ContextItem::Assistant {
        source: source(),
        replay: None,
        parts: folded_signed_message(&events, "answer, edited"),
    }]);
    let body = body_text(&edited);
    assert!(body.contains("dGhvdWdodA=="), "{body}");
    assert!(!body.contains("YW5zd2Vy"), "{body}");
    // Another model's message is never signed with this model's signatures.
    let foreign = request(vec![ContextItem::Assistant {
        source: ReplaySource {
            family: Family::GoogleGenerativeAi,
            model: "other-model".into(),
        },
        replay: None,
        parts: folded_signed_message(&events, "answer"),
    }]);
    let body = body_text(&foreign);
    assert!(!body.contains("thoughtSignature"), "{body}");
    // A stored signature that is not base64 never reaches the wire.
    let corrupt = request(vec![ContextItem::Assistant {
        source: source(),
        replay: None,
        parts: vec![
            AssistantPart::Text {
                text: "answer".into(),
                replay: None,
            },
            AssistantPart::Thinking {
                text: String::new().into(),
                replay: Some(raw(
                    r#"{"parts":[{"kind":"text","index":0,"bytes":6,"signature":"not base64!"}]}"#,
                )),
            },
        ],
    }]);
    let body = body_text(&corrupt);
    assert!(!body.contains("thoughtSignature"), "{body}");
}

#[test]
fn a_signature_only_part_after_a_call_signs_nothing() {
    let events = decode(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"checking"},{"functionCall":{"name":"read","args":{},"id":"call_1"}},{"text":"","thoughtSignature":"QUJD"}]},"finishReason":"STOP"}]}"#,
    ]);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Replay { .. })),
        "{events:?}"
    );
}

#[test]
fn flat_bodies_refuse_model_ids_that_leave_the_request_path() {
    for model in [
        "",
        "gemini-3-pro?alt=json",
        "gemini/../x",
        "tunedModels/m",
        "a b",
    ] {
        let request = ModelRequest {
            model: ModelRoute::Api {
                family: Family::GoogleGenerativeAi,
                model: model.into(),
            },
            ..request(vec![])
        };
        let thinking = plan(Some("high"));
        let error = request_body(&google_request(&request, &thinking)).unwrap_err();
        assert_eq!(
            error,
            GoogleBodyError::ModelId {
                model: model.into()
            }
        );
    }
}

#[test]
fn missing_and_reused_ids_get_fresh_unique_ids() {
    let events = decode(&[
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read"}},{"functionCall":{"name":"read"}}]},"finishReason":"STOP"}]}"#,
    ]);
    let (first, second) = match (&events[0], &events[1]) {
        (
            StreamEvent::ToolCallStarted { id: first, .. },
            StreamEvent::ToolCallStarted { id: second, .. },
        ) => (first.clone(), second.clone()),
        other => panic!("expected two tool starts, got {other:?}"),
    };
    assert!(first.starts_with("read_"), "{first}");
    assert!(second.starts_with("read_"), "{second}");
    assert_ne!(first, second);
}

#[test]
fn unnamed_function_call_is_a_protocol_error() {
    let mut decoder = GeminiDecoder::new(Family::GoogleVertex, "v", "gemini-3-pro", None);
    let mut out = Vec::new();
    let error = decoder
        .feed(
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"args":{}}}]}}]}"#,
            &mut out,
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "google-vertex sent an invalid stream: google functionCall part has no name"
    );
}

#[test]
fn end_without_finish_reason_cuts_the_stream() {
    let mut decoder = GeminiDecoder::new(Family::GoogleGenerativeAi, "g", "gemini-3-pro", None);
    let mut out = Vec::new();
    decoder
        .feed(
            r#"{"candidates":[{"content":{"parts":[{"text":"partial"}]}}]}"#,
            &mut out,
        )
        .unwrap();
    let error = decoder.end(&mut out).unwrap_err();
    assert!(matches!(error, ProviderError::StreamCut));
}

#[test]
fn malformed_chunk_names_the_family() {
    let mut decoder = GeminiDecoder::new(Family::GoogleAntigravity, "a", "gemini-3-pro", None);
    let mut out = Vec::new();
    let error = decoder.feed("{\"candidates\":[", &mut out).unwrap_err();
    assert!(matches!(error, ProviderError::Protocol { .. }));
    assert!(
        error
            .to_string()
            .starts_with("google-antigravity sent an invalid stream: ")
    );
}

#[test]
fn in_band_rate_limit_maps_through_the_retry_table() {
    let mut decoder = GeminiDecoder::new(Family::GoogleGenerativeAi, "g", "gemini-3-pro", None);
    let mut out = Vec::new();
    let error = decoder
        .feed(
            r#"{"error":{"code":429,"message":"Slow down","status":"RESOURCE_EXHAUSTED"}}"#,
            &mut out,
        )
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::RateLimited { .. }),
        "{error:?}"
    );
}

#[test]
fn blocked_prompt_with_no_candidates_refuses() {
    let mut decoder = GeminiDecoder::new(Family::GoogleGenerativeAi, "g", "gemini-3-pro", None);
    let mut out = Vec::new();
    let progress = decoder
        .feed(r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#, &mut out)
        .unwrap();
    assert_eq!(progress, Progress::Ended);
    assert!(matches!(
        out.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::Refusal,
        })
    ));
}

#[test]
fn safety_finish_refuses_and_malformed_errors() {
    let events = decode(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"x"}]},"finishReason":"SAFETY"}]}"#,
    ]);
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::Refusal,
        })
    ));

    let mut decoder = GeminiDecoder::new(Family::GoogleGenerativeAi, "g", "gemini-3-pro", None);
    let mut out = Vec::new();
    decoder
        .feed(
            r#"{"candidates":[{"content":{"parts":[]},"finishReason":"MALFORMED_FUNCTION_CALL"}]}"#,
            &mut out,
        )
        .unwrap();
    let error = decoder.end(&mut out).unwrap_err();
    assert!(matches!(error, ProviderError::Protocol { .. }), "{error:?}");
}

#[test]
fn cca_envelope_decodes_like_the_direct_response() {
    let events = decode(&[
        r#"{"response":{"candidates":[{"content":{"parts":[{"text":"Hi"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":1,"totalTokenCount":4},"responseId":"r1"}}"#,
    ]);
    assert!(matches!(
        &events[0],
        StreamEvent::TextDelta { text } if text == "Hi"
    ));
    assert!(matches!(
        &events[2],
        StreamEvent::Usage { usage } if usage.input_tokens == 3 && usage.output_tokens == 1
    ));
}

#[test]
fn response_id_reaches_the_session_slot() {
    let slot: ResponseIdSlot = Arc::default();
    let mut decoder = GeminiDecoder::new(
        Family::GoogleAntigravity,
        "a",
        "gemini-3-pro",
        Some(slot.clone()),
    );
    let mut out = Vec::new();
    decoder
        .feed(
            r#"{"response":{"candidates":[{"content":{"parts":[]}}],"responseId":"abc123"}}"#,
            &mut out,
        )
        .unwrap();
    assert_eq!(slot.lock().unwrap().as_deref(), Some("abc123"));
}

#[test]
fn decode_events_ends_through_end_of_input() {
    let events = vec![
        Ok(SseEvent {
            name: None,
            data: r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}]}}]}"#.into(),
        }),
        Ok(SseEvent {
            name: None,
            data: r#"{"candidates":[{"finishReason":"STOP"}]}"#.into(),
        }),
    ];
    let collected: Vec<_> = block_on(
        super::decode_events(
            Family::GoogleGenerativeAi,
            "google",
            "gemini-3-pro",
            futures::stream::iter(events),
            None,
        )
        .collect(),
    );
    assert!(collected.iter().all(Result::is_ok));
    assert!(matches!(
        collected.last(),
        Some(Ok(StreamEvent::Stop { .. }))
    ));
}

// ---------------------------------------------------------------------------
// HTTP error bodies
// ---------------------------------------------------------------------------

fn quota_body() -> &'static str {
    r#"{"error":{"code":429,"message":"Quota exceeded for quota metric 'GenerateContent requests'","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"QUOTA_EXHAUSTED"}]}}"#
}

fn request_state() -> RequestState<'static> {
    RequestState {
        family: Family::GoogleGenerativeAi,
        provider: "google",
        model: "gemini-3-pro",
        oauth: false,
        refreshed: false,
        delivered: false,
    }
}

#[test]
fn quota_details_classify_as_quota() {
    let error = classify_http(429, quota_body(), &request_state());
    assert!(matches!(error, ProviderError::Quota { .. }), "{error:?}");
}

#[test]
fn bare_throttle_stays_rate_limited() {
    let error = classify_http(
        429,
        r#"{"error":{"code":429,"message":"Slow down","status":"RESOURCE_EXHAUSTED"}}"#,
        &request_state(),
    );
    assert!(
        matches!(error, ProviderError::RateLimited { .. }),
        "{error:?}"
    );
}

#[test]
fn invalid_argument_is_an_invalid_request() {
    let error = classify_http(
        400,
        r#"{"error":{"code":400,"message":"Bad tokens","status":"INVALID_ARGUMENT"}}"#,
        &request_state(),
    );
    assert!(
        matches!(error, ProviderError::InvalidRequest { .. }),
        "{error:?}"
    );
}

#[test]
fn error_fields_fall_back_to_the_first_line() {
    let (code, message) = error_fields(quota_body());
    assert_eq!(code.as_deref(), Some("RESOURCE_EXHAUSTED"));
    assert!(message.starts_with("Quota exceeded"), "{message}");
    let (code, message) = error_fields("<html>down</html>\nmore");
    assert_eq!(code, None);
    assert_eq!(message, "<html>down</html>");
}

// ---------------------------------------------------------------------------
// Loopback turns: build -> send -> SSE bytes -> decode_events over a real
// socket. The scripted server speaks raw HTTP and records every request
// head and body, so the header, path, and body assertions witness what the
// server actually received; the collected events witness what the client
// decoded. Nothing here reaches past loopback.
// ---------------------------------------------------------------------------

use std::sync::Mutex as LoopbackMutex;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener as LoopbackListener,
};

use crate::family::google::{antigravity, gemini, vertex};

/// One captured request: the raw head and the exact body bytes.
type Seen = Arc<LoopbackMutex<Vec<(String, Vec<u8>)>>>;

fn loopback_request(family: Family, model: &str) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family,
            model: model.into(),
        },
        system: "You are terse.".into(),
        tools: Vec::new().into(),
        context: vec![ContextItem::User {
            parts: vec![text("Hi")],
        }]
        .into(),
        params: RequestParams {
            thinking: ThinkingLevel::High,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

async fn read_captured(socket: &mut tokio::net::TcpStream) -> std::io::Result<(String, Vec<u8>)> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        socket.read_exact(&mut byte).await?;
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let length = String::from_utf8_lossy(&head)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or_default();
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await?;
    Ok((String::from_utf8_lossy(&head).into_owned(), body))
}

fn http_bytes(status: u16, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn sse(chunks: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in chunks {
        out.extend_from_slice(b"data: ");
        out.extend_from_slice(chunk.as_bytes());
        out.extend_from_slice(b"\n\n");
    }
    out
}

async fn serve_script(
    listener: LoopbackListener,
    script: Vec<(u16, Vec<u8>)>,
    seen: Seen,
) -> std::io::Result<()> {
    for (status, body) in script {
        let (mut socket, _) = listener.accept().await?;
        let (head, req_body) = read_captured(&mut socket).await?;
        seen.lock().unwrap().push((head, req_body));
        socket.write_all(&http_bytes(status, &body)).await?;
    }
    Ok(())
}

async fn collect_turn(
    family: Family,
    provider: &str,
    model: &str,
    response: reqwest::Response,
) -> Vec<Result<StreamEvent, ProviderError>> {
    let chunks = response
        .bytes_stream()
        .map(|chunk| chunk.expect("loopback chunk").to_vec());
    decode_events(
        family,
        provider,
        model,
        crate::sse::decode_stream(chunks),
        None,
    )
    .collect()
    .await
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one loopback fixture proves the direct turn end to end in place"
)]
async fn loopback_direct_turn_carries_key_and_decodes_tool_reasoning() {
    let mut request = loopback_request(Family::GoogleGenerativeAi, "gemini-3-pro");
    request.tools = vec![ModelToolSpec {
        name: "read".into(),
        description: "Read a file.".into(),
        parameters: raw(r#"{"type":"object"}"#),
        grammar: None,
        defer_loading: false,
    }]
    .into();
    request.context = vec![
        ContextItem::User {
            parts: vec![text("Hi")],
        },
        ContextItem::Assistant {
            source: source(),
            replay: None,
            parts: vec![AssistantPart::ToolCall {
                call: CallId::new("call_1"),
                name: "read".into(),
                args: raw(r#"{"path":"a.txt"}"#),
                replay: None,
            }],
        },
        ContextItem::ToolResult {
            call: CallId::new("call_1"),
            name: "read".into(),
            is_error: false,
            parts: vec![text("ok")],
        },
    ]
    .into();
    let thinking = plan(Some("high"));
    let wire = gemini::build(&gemini::GeminiRequest {
        request: &request,
        thinking: &thinking,
        api_key: "secret-key",
        image_input: true,
    })
    .unwrap();

    let body = sse(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"Hello"}]}}]}"#,
        r#"{"candidates":[{"content":{"parts":[{"text":" world","thought":true}]}}]}"#,
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{"path":"a.txt"},"id":"call_1"},"thoughtSignature":"QUJDRA=="}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"thoughtsTokenCount":2,"totalTokenCount":17}}"#,
    ]);
    let listener = LoopbackListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let mut server = tokio::task::JoinSet::new();
    server.spawn(serve_script(listener, vec![(200, body)], Arc::clone(&seen)));

    let response = send(
        Family::GoogleGenerativeAi,
        &crate::http::build_client(),
        wire,
        &base,
        "dalgon/test",
    )
    .await
    .unwrap();
    assert!(response.status().is_success());
    let events = collect_turn(
        Family::GoogleGenerativeAi,
        "google",
        "gemini-3-pro",
        response,
    )
    .await;
    server
        .join_next()
        .await
        .expect("server completes")
        .expect("serve ok")
        .unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let (head, req_body) = &seen[0];
    assert!(
        head.contains("POST /models/gemini-3-pro:streamGenerateContent?alt=sse HTTP/1.1"),
        "{head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("x-goog-api-key: secret-key"),
        "{head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("accept: text/event-stream"),
        "{head}"
    );
    let req_body = String::from_utf8_lossy(req_body).into_owned();
    assert!(req_body.contains("\"contents\""), "{req_body}");
    assert!(
        req_body.contains("\"thoughtSignature\":\"skip_thought_signature_validator\""),
        "{req_body}"
    );
    assert!(
        req_body.contains("\"parametersJsonSchema\":{\"type\":\"object\",\"properties\":{}}"),
        "{req_body}"
    );

    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(
        matches!(&events[0], Ok(StreamEvent::TextDelta { text }) if text == "Hello"),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            Ok(StreamEvent::ReasoningDelta { text }) if text == " world"
        )),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            Ok(StreamEvent::ToolCallStarted { id, name }) if id == "call_1" && name == "read"
        )),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            Ok(StreamEvent::Replay { payload })
                if payload.item.as_str() == r#"{"calls":{"call_1":"QUJDRA=="}}"#
        )),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            Ok(StreamEvent::Usage { usage })
                if usage.input_tokens == 10
                    && usage.output_tokens == 5
                    && usage.reasoning_tokens == Some(2)
        )),
        "{events:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(Ok(StreamEvent::Stop {
                reason: StopReason::ToolUse,
            }))
        ),
        "{events:?}"
    );
}

#[tokio::test]
async fn loopback_vertex_bearer_targets_the_project_path() {
    let request = loopback_request(Family::GoogleVertex, "gemini-3-pro");
    let thinking = plan(Some("high"));
    let mut wire = vertex::build(&vertex::VertexRequest {
        request: &request,
        thinking: &thinking,
        auth: vertex::VertexAuth::Bearer {
            access_token: "token",
            project: "proj-1",
            location: "us-central1",
        },
        image_input: true,
    })
    .unwrap();
    wire.base_override = None;

    let body = sse(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}]}}]}"#,
        r#"{"candidates":[{"finishReason":"STOP"}]}"#,
    ]);
    let listener = LoopbackListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let mut server = tokio::task::JoinSet::new();
    server.spawn(serve_script(listener, vec![(200, body)], Arc::clone(&seen)));

    let response = send(
        Family::GoogleVertex,
        &crate::http::build_client(),
        wire,
        &base,
        "dalgon/test",
    )
    .await
    .unwrap();
    let events = collect_turn(Family::GoogleVertex, "vertex", "gemini-3-pro", response).await;
    server
        .join_next()
        .await
        .expect("server completes")
        .expect("serve ok")
        .unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let (head, req_body) = &seen[0];
    assert!(
        head.contains(
            "POST /v1/projects/proj-1/locations/us-central1/publishers/google/models/gemini-3-pro:streamGenerateContent?alt=sse HTTP/1.1"
        ),
        "{head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer token"),
        "{head}"
    );
    let req_body = String::from_utf8_lossy(req_body).into_owned();
    assert!(req_body.contains("\"threshold\":\"OFF\""), "{req_body}");
    assert!(!req_body.contains("thoughtSignature"), "{req_body}");

    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(
        matches!(&events[0], Ok(StreamEvent::TextDelta { text }) if text == "Hi"),
        "{events:?}"
    );
    assert!(
        matches!(events.last(), Some(Ok(StreamEvent::Stop { .. }))),
        "{events:?}"
    );
}

#[tokio::test]
async fn loopback_vertex_express_falls_back_from_regional_to_global() {
    let request = loopback_request(Family::GoogleVertex, "gemini-3-pro");
    let thinking = plan(Some("high"));
    let mut wire = vertex::build(&vertex::VertexRequest {
        request: &request,
        thinking: &thinking,
        auth: vertex::VertexAuth::Express {
            api_key: "key",
            location: Some("europe-west1"),
            explicit_location: false,
        },
        image_input: true,
    })
    .unwrap();
    assert!(wire.fallback.is_some());
    wire.base_override = None;
    if let Some(fallback) = wire.fallback.as_mut() {
        fallback.base_override = None;
    }

    let body = sse(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"global"}]}}]}"#,
        r#"{"candidates":[{"finishReason":"STOP"}]}"#,
    ]);
    let listener = LoopbackListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let mut server = tokio::task::JoinSet::new();
    server.spawn(serve_script(
        listener,
        vec![
            (
                404,
                br#"{"error":{"code":404,"message":"not here","status":"NOT_FOUND"}}"#.to_vec(),
            ),
            (200, body),
        ],
        Arc::clone(&seen),
    ));

    let response = send(
        Family::GoogleVertex,
        &crate::http::build_client(),
        wire,
        &base,
        "dalgon/test",
    )
    .await
    .unwrap();
    let events = collect_turn(Family::GoogleVertex, "vertex", "gemini-3-pro", response).await;
    server
        .join_next()
        .await
        .expect("server completes")
        .expect("serve ok")
        .unwrap();

    // Both attempts carried the Express key on the publishers path; the
    // regional 404 failed over instead of ending the turn.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for (head, _) in seen.iter() {
        assert!(
            head.contains(
                "POST /v1/publishers/google/models/gemini-3-pro:streamGenerateContent?alt=sse HTTP/1.1"
            ),
            "{head}"
        );
        assert!(
            head.to_ascii_lowercase().contains("x-goog-api-key: key"),
            "{head}"
        );
    }
    assert!(
        matches!(&events[0], Ok(StreamEvent::TextDelta { text }) if text == "global"),
        "{events:?}"
    );
    assert!(
        matches!(events.last(), Some(Ok(StreamEvent::Stop { .. }))),
        "{events:?}"
    );
}

#[tokio::test]
async fn loopback_antigravity_posts_the_cca_envelope() {
    let request = loopback_request(Family::GoogleAntigravity, "gemini-3-flash-agent");
    let thinking = plan(Some("high"));
    let mut session = antigravity::AntigravitySession::default();
    let envelope = session.envelope(Some("MODEL_PLACEHOLDER_M132"), false);
    let mut wire = antigravity::build(&antigravity::AntigravityRequest {
        request: &request,
        thinking: &thinking,
        access_token: "token",
        project: "proj",
        wire_model: "gemini-3-flash-agent",
        client_version: "2.19.1",
        envelope: &envelope,
        claude_thinking: false,
        image_input: true,
    })
    .unwrap();
    wire.base_override = None;

    let body = sse(&[
        r#"{"response":{"candidates":[{"content":{"parts":[{"text":"Hi"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":1,"totalTokenCount":4},"responseId":"r1"}}"#,
    ]);
    let listener = LoopbackListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let mut server = tokio::task::JoinSet::new();
    server.spawn(serve_script(listener, vec![(200, body)], Arc::clone(&seen)));

    let response = send(
        Family::GoogleAntigravity,
        &crate::http::build_client(),
        wire,
        &base,
        "dalgon/test",
    )
    .await
    .unwrap();
    let events = collect_turn(
        Family::GoogleAntigravity,
        "antigravity",
        "gemini-3-flash-agent",
        response,
    )
    .await;
    server
        .join_next()
        .await
        .expect("server completes")
        .expect("serve ok")
        .unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let (head, req_body) = &seen[0];
    assert!(
        head.contains("POST /v1internal:streamGenerateContent?alt=sse HTTP/1.1"),
        "{head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer token"),
        "{head}"
    );
    assert!(head.contains("antigravity/hub/2.19.1"), "{head}");
    let req_body = String::from_utf8_lossy(req_body).into_owned();
    for member in [
        "\"project\":\"proj\"",
        "\"userAgent\":\"antigravity\"",
        "\"requestType\":\"agent\"",
        "MODEL_PLACEHOLDER_M132",
    ] {
        assert!(req_body.contains(member), "{req_body}");
    }

    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(
        matches!(&events[0], Ok(StreamEvent::TextDelta { text }) if text == "Hi"),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            Ok(StreamEvent::Usage { usage })
                if usage.input_tokens == 3 && usage.output_tokens == 1
        )),
        "{events:?}"
    );
    assert!(
        matches!(events.last(), Some(Ok(StreamEvent::Stop { .. }))),
        "{events:?}"
    );
}

#[tokio::test]
async fn loopback_empty_stop_is_re_requested_through_the_lifecycle() {
    use crate::{
        auth::credential::{Credential, OAuthCredential},
        lifecycle,
        stream::EventStream,
    };
    use tokio_util::sync::CancellationToken;

    // The first response is the empty-STOP failure; the second carries the
    // answer. The decoder errors before delivering anything, so the
    // lifecycle's pre-delivery retry re-requests and the consumer only ever
    // sees the second response.
    let empty =
        sse(&[r#"{"candidates":[{"content":{"parts":[{"text":""}]},"finishReason":"STOP"}]}"#]);
    let answer = sse(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"Hello"}]},"finishReason":"STOP"}]}"#,
    ]);
    let listener = LoopbackListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let mut server = tokio::task::JoinSet::new();
    server.spawn(serve_script(
        listener,
        vec![(200, empty), (200, answer)],
        Arc::clone(&seen),
    ));

    let request = loopback_request(Family::GoogleGenerativeAi, "gemini-3-pro");
    let thinking = plan(Some("high"));
    let client = crate::http::build_client();
    let attempt = move |_: &Credential, _: &CancellationToken| {
        let (request, thinking, client, base) = (
            request.clone(),
            thinking.clone(),
            client.clone(),
            base.clone(),
        );
        async move {
            let wire = gemini::build(&gemini::GeminiRequest {
                request: &request,
                thinking: &thinking,
                api_key: "secret-key",
                image_input: true,
            })
            .map_err(lifecycle::AttemptFailure::Provider)?;
            let response = send(
                Family::GoogleGenerativeAi,
                &client,
                wire,
                &base,
                "dalgon/test",
            )
            .await
            .map_err(lifecycle::AttemptFailure::Provider)?;
            let chunks = response
                .bytes_stream()
                .map(|chunk| chunk.expect("loopback chunk").to_vec());
            let events = decode_events(
                Family::GoogleGenerativeAi,
                "google",
                "gemini-3-pro",
                crate::sse::decode_stream(chunks),
                None,
            );
            Ok(Some(EventStream::new(events, || {})))
        }
    };
    let refresh = |_: OAuthCredential| async {
        Err::<Credential, _>(ProviderError::NoCredentials {
            provider: String::from("google"),
        })
    };
    let plan = lifecycle::Plan::new(
        Family::GoogleGenerativeAi,
        "google",
        "gemini-3-pro",
        2,
        Arc::new(tokio::sync::Semaphore::new(1)),
        CancellationToken::new(),
        Arc::new(|_: String| {}),
    );
    let mut stream = lifecycle::stream(plan, Credential::None, attempt, refresh);
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.expect("the retried response succeeds"));
    }
    server
        .join_next()
        .await
        .expect("server completes")
        .expect("serve ok")
        .unwrap();

    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "the empty response was re-requested"
    );
    assert_eq!(
        events[0],
        StreamEvent::TextDelta {
            text: String::from("Hello"),
        }
    );
    assert!(
        matches!(
            events.last(),
            Some(StreamEvent::Stop {
                reason: StopReason::EndTurn,
            })
        ),
        "{events:?}"
    );
}

#[tokio::test]
async fn loopback_rejection_maps_quota_through_the_classifier() {
    let request = loopback_request(Family::GoogleGenerativeAi, "gemini-3-pro");
    let thinking = plan(Some("high"));
    let wire = gemini::build(&gemini::GeminiRequest {
        request: &request,
        thinking: &thinking,
        api_key: "secret-key",
        image_input: true,
    })
    .unwrap();

    let listener = LoopbackListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let mut server = tokio::task::JoinSet::new();
    server.spawn(serve_script(
        listener,
        vec![(429, quota_body().as_bytes().to_vec())],
        Arc::clone(&seen),
    ));

    let response = send(
        Family::GoogleGenerativeAi,
        &crate::http::build_client(),
        wire,
        &base,
        "dalgon/test",
    )
    .await
    .unwrap();
    assert_eq!(response.status().as_u16(), 429);
    let body = crate::http::read_body(Family::GoogleGenerativeAi, response)
        .await
        .unwrap();
    let error = classify_http(429, &String::from_utf8_lossy(&body), &request_state());
    assert!(matches!(error, ProviderError::Quota { .. }), "{error:?}");
    server
        .join_next()
        .await
        .expect("server completes")
        .expect("serve ok")
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn dropped_decode_stream_never_polls_the_pending_tail() {
    let first: Result<SseEvent, ProviderError> = Ok(SseEvent {
        name: None,
        data: r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}]}}]}"#.into(),
    });
    let events = futures::stream::iter(vec![first])
        .chain(futures::stream::pending::<Result<SseEvent, ProviderError>>());
    let mut stream = Box::pin(decode_events(
        Family::GoogleGenerativeAi,
        "google",
        "gemini-3-pro",
        events,
        None,
    ));
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        futures::StreamExt::next(&mut stream),
    )
    .await
    .expect("the first event arrives without the tail");
    assert!(
        matches!(next, Some(Ok(StreamEvent::TextDelta { .. }))),
        "{next:?}"
    );
    // Cancellation is a drop: it must neither hang nor poll the pending tail.
    drop(stream);
}

#[tokio::test]
async fn closed_stream_before_any_first_event_cuts_with_error() {
    // The transport closing before any first event is a typed cut, never a
    // hang and never a silent empty turn: `end` without a `finishReason`
    // refuses the stream.
    let stream = decode_events(
        Family::GoogleGenerativeAi,
        "google",
        "gemini-3-pro",
        futures::stream::iter(Vec::<Result<SseEvent, ProviderError>>::new()),
        None,
    );
    let collected = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        Box::pin(stream).collect::<Vec<_>>(),
    )
    .await
    .expect("a closed stream terminates instead of hanging");
    assert!(
        matches!(collected.last(), Some(Err(ProviderError::StreamCut))),
        "{collected:?}"
    );
}

#[test]
fn wire_debug_redacts_credentials() {
    let request = loopback_request(Family::GoogleGenerativeAi, "gemini-3-pro");
    let thinking = plan(Some("high"));
    let wire = gemini::build(&gemini::GeminiRequest {
        request: &request,
        thinking: &thinking,
        api_key: "secret-key",
        image_input: true,
    })
    .unwrap();
    let rendered = format!("{wire:?}");
    assert!(!rendered.contains("secret-key"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
}
