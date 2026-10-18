use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use dal_core::{CallId, ModelToolSpec, Purpose, ReplaySource, RequestParams, ThinkingLevel};
use futures::executor::block_on;

use super::*;

const NO_USAGE: Usage = Usage {
    input_tokens: 0,
    cached_input_tokens: 0,
    output_tokens: 0,
    reasoning_tokens: None,
    cache_write_tokens: 0,
    premium_requests: None,
    cost_usd: None,
};
fn raw(text: &str) -> RawJson {
    RawJson::parse(text).unwrap()
}

fn replay_source() -> ReplaySource {
    ReplaySource {
        family: Family::Ollama,
        model: "qwen3:8b".into(),
    }
}

fn png() -> Part {
    Part::Image {
        mime: "image/png".into(),
        bytes: b"\x89PNG".as_slice().into(),
    }
}

fn text(value: &str) -> Part {
    Part::Text { text: value.into() }
}

fn plan(effort: Option<&'static str>, temperature: Option<f32>) -> ThinkingPlan {
    ThinkingPlan {
        level: ThinkingLevel::High,
        wire: WireThinking::OpenAi { effort },
        temperature,
        notices: Vec::new(),
    }
}

fn request(tools: Vec<ModelToolSpec>, context: Vec<ContextItem>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Ollama,
            model: "qwen3:8b".into(),
        },
        system: "You are terse.".into(),
        tools: tools.into(),
        context: context.into(),
        params: RequestParams {
            thinking: ThinkingLevel::High,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: Some("0190-session".into()),
    }
}

fn tool(name: &str, description: &str, parameters: &str) -> ModelToolSpec {
    ModelToolSpec {
        name: name.into(),
        description: description.into(),
        parameters: raw(parameters),
        grammar: None,
        defer_loading: false,
    }
}

/// A named-level ladder, the shape every string-valued model carries.
fn levels() -> ThinkingSupport {
    ThinkingSupport::OpenAi {
        accepted: vec![ThinkingLevel::Low, ThinkingLevel::Medium, ThinkingLevel::High],
        none_supported: true,
    }
}

fn body_with(request: &ModelRequest, thinking: &ThinkingPlan, policy: WirePolicy) -> String {
    String::from_utf8(request_body(request, thinking, policy).unwrap()).unwrap()
}

fn body_text(request: &ModelRequest, thinking: &ThinkingPlan) -> String {
    body_with(request, thinking, WirePolicy::SELF_HOSTED)
}

#[test]
fn request_body_matches_the_member_order_byte_for_byte() {
    let mut request = request(
        vec![
            tool(
                "read",
                "Read a file.",
                r#"{"type":"object","properties":{"path":{"type":"string"}}}"#,
            ),
            tool("list", "List files.", r#"{"type": "object"}"#),
        ],
        vec![
            ContextItem::User {
                parts: vec![text("Describe"), png()],
            },
            ContextItem::Assistant {
                source: replay_source(),
                replay: None,
                parts: vec![
                    AssistantPart::Thinking {
                        text: "hidden".into(),
                        replay: None,
                    },
                    AssistantPart::Text {
                        text: "Checking.".into(),
                        replay: None,
                    },
                    AssistantPart::ToolCall {
                        call: CallId::new("ollama:0:read"),
                        name: "read".into(),
                        args: raw(r#"{"path": "a.txt" ,"n":1.50}"#),
                        replay: None,
                    },
                ],
            },
            ContextItem::ToolResult {
                call: CallId::new("ollama:0:read"),
                name: "read".into(),
                is_error: false,
                parts: vec![text("line1"), text("line2"), png()],
            },
        ],
    );
    request.params.max_output_tokens = Some(256);
    let expected = concat!(
        r#"{"model":"qwen3:8b","messages":["#,
        r#"{"role":"system","content":"You are terse."},"#,
        r#"{"role":"user","content":"Describe","images":["iVBORw=="]},"#,
        r#"{"role":"assistant","content":"Checking.","thinking":"hidden","#,
        r#""tool_calls":[{"type":"function","function":{"name":"read","#,
        r#""arguments":{"path": "a.txt" ,"n":1.50}}}]},"#,
        r#"{"role":"tool","content":"line1\nline2","tool_name":"read","images":["iVBORw=="]}],"#,
        r#""tools":[{"type":"function","function":{"name":"read","description":"Read a file.","#,
        r#""parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},"#,
        r#"{"type":"function","function":{"name":"list","description":"List files.","#,
        r#""parameters":{"type": "object"}}}],"#,
        r#""think":"high","options":{"num_predict":256,"temperature":0.5},"stream":true}"#,
    );
    assert_eq!(
        body_text(&request, &plan(Some("high"), Some(0.5))),
        expected
    );
    // Two builds of the same request are equal byte for byte.
    assert_eq!(
        request_body(
            &request,
            &plan(Some("high"), Some(0.5)),
            WirePolicy::SELF_HOSTED
        ),
        request_body(
            &request,
            &plan(Some("high"), Some(0.5)),
            WirePolicy::SELF_HOSTED
        )
    );
}

#[test]
fn request_body_without_tools_think_or_options_omits_those_members() {
    let request = request(Vec::new(), Vec::new());
    assert_eq!(
        body_text(&request, &plan(None, None)),
        r#"{"model":"qwen3:8b","messages":[{"role":"system","content":"You are terse."}],"stream":true}"#
    );
}

#[test]
fn think_spells_the_pinned_effort_ladder() {
    let mut request = request(Vec::new(), Vec::new());
    request.system = "".into();
    for (effort, member) in [
        ("none", r#""think":false,"#),
        ("minimal", r#""think":"low","#),
        ("low", r#""think":"low","#),
        ("medium", r#""think":"medium","#),
        ("high", r#""think":"high","#),
        // The planner only yields `xhigh` for a model whose server listed it
        // in `thinking.values`, so it goes out as the name the server gave.
        ("xhigh", r#""think":"xhigh","#),
        ("max", r#""think":"max","#),
    ] {
        let expected = format!(r#"{{"model":"qwen3:8b","messages":[],{member}"stream":true}}"#);
        assert_eq!(body_text(&request, &plan(Some(effort), None)), expected);
    }
    // A spelling the ladder does not know omits the member rather than
    // sending something the server never advertised.
    assert_eq!(
        body_text(&request, &plan(Some("ludicrous"), None)),
        r#"{"model":"qwen3:8b","messages":[],"stream":true}"#
    );
}

#[test]
fn request_body_rejects_what_ollama_cannot_carry() {
    let blob_id = BlobId::from_bytes(b"stored");
    let blob = request(
        Vec::new(),
        vec![ContextItem::User {
            parts: vec![Part::Blob {
                blob_id,
                mime: "image/png".into(),
                bytes: 6,
            }],
        }],
    );
    assert_eq!(
        request_body(&blob, &plan(None, None), WirePolicy::SELF_HOSTED),
        Err(OllamaBodyError::UnresolvedBlob { blob_id })
    );

    let mut other = request(Vec::new(), Vec::new());
    other.model = ModelRoute::Api {
        family: Family::Chat,
        model: "qwen3:8b".into(),
    };
    assert_eq!(
        request_body(&other, &plan(None, None), WirePolicy::SELF_HOSTED),
        Err(OllamaBodyError::NotOllama {
            route: "qwen3:8b".into()
        })
    );

    let anthropic = ThinkingPlan {
        wire: WireThinking::Anthropic {
            thinking: crate::thinking::AnthropicThinking::Adaptive,
            effort: None,
        },
        ..plan(None, None)
    };
    assert_eq!(
        request_body(
            &request(Vec::new(), Vec::new()),
            &anthropic,
            WirePolicy::SELF_HOSTED
        ),
        Err(OllamaBodyError::ForeignThinking)
    );
}

#[test]
fn an_assistant_message_with_nothing_to_send_is_omitted() {
    let request = request(
        Vec::new(),
        vec![
            ContextItem::Assistant {
                source: replay_source(),
                replay: None,
                parts: Vec::new(),
            },
            ContextItem::Assistant {
                source: replay_source(),
                replay: None,
                parts: vec![AssistantPart::Thinking {
                    text: "only thoughts".into(),
                    replay: None,
                }],
            },
        ],
    );
    // A cloud model that rejects replayed reasoning leaves a thinking-only
    // message with nothing to send, so the message is omitted entirely.
    let cloud = WirePolicy::for_model(Deployment::Cloud, "qwen3:8b", &levels());
    let body = body_with(&request, &plan(None, None), cloud);
    assert_eq!(
        body,
        r#"{"model":"qwen3:8b","messages":[{"role":"system","content":"You are terse."}],"stream":true}"#
    );
    assert!(!body.contains("thinking"), "{body}");
}

fn thinking_history() -> ModelRequest {
    request(
        Vec::new(),
        vec![ContextItem::Assistant {
            source: replay_source(),
            replay: None,
            parts: vec![
                AssistantPart::Thinking {
                    text: "hidden".into(),
                    replay: None,
                },
                AssistantPart::Text {
                    text: "Checking.".into(),
                    replay: None,
                },
            ],
        }],
    )
}

#[test]
fn a_self_hosted_server_takes_assistant_thinking_back_in_history() {
    // The resolved reference compat defaults `stripThinkingHistory` to false:
    // only the cloud rows opt in to stripping, so a local server replays.
    let body = body_text(&thinking_history(), &plan(None, None));
    assert_eq!(
        body,
        concat!(
            r#"{"model":"qwen3:8b","messages":[{"role":"system","content":"You are terse."},"#,
            r#"{"role":"assistant","content":"Checking.","thinking":"hidden"}],"stream":true}"#,
        )
    );
}

#[test]
fn cloud_models_drop_assistant_thinking_except_glm_5_2() {
    // The cloud rows carry `strip-thinking-history`; GLM 5.2 and its
    // `:cloud` wire id are the verified exceptions. Text still replays.
    for model in ["qwen3:8b", "gpt-oss:120b", "glm-5.1", "glm-5.2:turbo"] {
        let policy = WirePolicy::for_model(Deployment::Cloud, model, &levels());
        let body = body_with(&thinking_history(), &plan(None, None), policy);
        assert_eq!(
            body,
            concat!(
                r#"{"model":"qwen3:8b","messages":[{"role":"system","content":"You are terse."},"#,
                r#"{"role":"assistant","content":"Checking."}],"stream":true}"#,
            ),
            "{model}"
        );
        assert!(!body.contains("hidden"), "{model}: {body}");
    }
    for model in ["glm-5.2", "glm-5.2:cloud", "GLM-5.2"] {
        let policy = WirePolicy::for_model(Deployment::Cloud, model, &levels());
        let body = body_with(&thinking_history(), &plan(None, None), policy);
        assert!(body.contains(r#""thinking":"hidden""#), "{model}: {body}");
    }
}

#[test]
fn the_cloud_holds_num_predict_to_its_enforced_ceiling() {
    let mut request = request(Vec::new(), Vec::new());
    let cloud = WirePolicy::for_model(Deployment::Cloud, "deepseek-v4-flash", &levels());
    for (asked, sent) in [(70_000_u32, 65_536_u32), (65_536, 65_536), (1_024, 1_024)] {
        request.params.max_output_tokens = Some(asked);
        let body = body_with(&request, &plan(None, None), cloud);
        assert!(
            body.contains(&format!(r#""options":{{"num_predict":{sent}}}"#)),
            "{asked}: {body}"
        );
        // A self-hosted server never sees the cloud ceiling.
        let local = body_with(&request, &plan(None, None), WirePolicy::SELF_HOSTED);
        assert!(
            local.contains(&format!(r#""options":{{"num_predict":{asked}}}"#)),
            "{asked}: {local}"
        );
    }
    // No caller limit means no `num_predict`, cloud or not.
    request.params.max_output_tokens = None;
    assert_eq!(
        body_with(&request, &plan(None, None), cloud),
        r#"{"model":"qwen3:8b","messages":[{"role":"system","content":"You are terse."}],"stream":true}"#
    );
}

#[test]
fn a_boolean_only_model_spells_every_reasoning_level_true() {
    // `thinking.values` of `[false, true]` or `[true]`: no levels, so the
    // ladder is the single boolean level and the wire says `true`.
    let boolean = ThinkingSupport::OpenAi {
        accepted: vec![BOOLEAN_ON],
        none_supported: true,
    };
    assert!(is_boolean_ladder(&boolean));
    assert!(!is_boolean_ladder(&levels()));
    // `minimal` plus real levels is a named ladder, not the boolean one.
    assert!(!is_boolean_ladder(&ThinkingSupport::OpenAi {
        accepted: vec![ThinkingLevel::Minimal, ThinkingLevel::Low],
        none_supported: true,
    }));
    let policy = WirePolicy::for_model(Deployment::SelfHosted, "qwen3:0.6b", &boolean);
    assert!(policy.boolean_think);
    let mut request = request(Vec::new(), Vec::new());
    request.system = "".into();
    for (effort, member) in [
        ("none", r#""think":false,"#),
        ("minimal", r#""think":true,"#),
        ("medium", r#""think":true,"#),
        ("max", r#""think":true,"#),
    ] {
        let expected = format!(r#"{{"model":"qwen3:8b","messages":[],{member}"stream":true}}"#);
        assert_eq!(
            body_with(&request, &plan(Some(effort), None), policy),
            expected,
            "{effort}"
        );
    }
    // No effort at all still omits the member, so an always-on model keeps
    // its server default.
    assert_eq!(
        body_with(&request, &plan(None, None), policy),
        r#"{"model":"qwen3:8b","messages":[],"stream":true}"#
    );
}

#[test]
fn a_provider_row_is_cloud_by_id_or_by_host() {
    for (provider, base, expected) in [
        ("ollama-cloud", "https://ollama.com", Deployment::Cloud),
        // The built-in cloud row keeps the cloud rules behind any proxy.
        ("ollama-cloud", "http://127.0.0.1:9000", Deployment::Cloud),
        ("mine", "https://ollama.com/api", Deployment::Cloud),
        ("mine", "https://eu.OLLAMA.com", Deployment::Cloud),
        ("ollama", "http://127.0.0.1:11434", Deployment::SelfHosted),
        ("mine", "https://notollama.com", Deployment::SelfHosted),
        ("mine", "https://ollama.com.evil.test", Deployment::SelfHosted),
        ("mine", "not a url", Deployment::SelfHosted),
    ] {
        assert_eq!(Deployment::of(provider, base), expected, "{provider} {base}");
    }
}

#[test]
fn mapped_tool_names_apply_to_advertisements_and_history() {
    let request = request(
        vec![tool("fs.read", "Read.", r#"{"type":"object"}"#)],
        vec![ContextItem::Assistant {
            source: replay_source(),
            replay: None,
            parts: vec![AssistantPart::ToolCall {
                call: CallId::new("ollama:0:fs.read"),
                name: "fs.read".into(),
                args: raw("{}"),
                replay: None,
            }],
        }],
    );
    let body = body_text(&request, &plan(None, None));
    let wire = wire_name("fs.read", WIRE_NAME_MAX);
    assert_ne!(wire, "fs.read");
    assert!(!body.contains("fs.read"), "{body}");
    assert_eq!(
        body.matches(format!(r#""name":"{wire}""#).as_str()).count(),
        2,
        "{body}"
    );
}

#[test]
fn trim_api_suffix_accepts_bases_spelled_either_way() {
    assert_eq!(
        trim_api_suffix("http://localhost:11434"),
        "http://localhost:11434"
    );
    assert_eq!(
        trim_api_suffix("http://localhost:11434/"),
        "http://localhost:11434"
    );
    assert_eq!(
        trim_api_suffix("http://localhost:11434/api"),
        "http://localhost:11434"
    );
    assert_eq!(
        trim_api_suffix("https://ollama.com/api/"),
        "https://ollama.com"
    );
    assert_eq!(
        trim_api_suffix("https://proxy.test/ollama"),
        "https://proxy.test/ollama"
    );
}

fn ndjson(records: &[&str]) -> String {
    let mut out = String::new();
    for record in records {
        out.push_str(record);
        out.push('\n');
    }
    out
}

/// Runs `wire` through the NDJSON framing and the Ollama decoder, delivered in
/// network chunks of `split` bytes.
fn run(wire: &str, split: usize) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let chunks: Vec<Vec<u8>> = wire.as_bytes().chunks(split).map(<[u8]>::to_vec).collect();
    let decoded = decode_stream(stream::iter(chunks), OllamaDecoder::new());
    let items: Vec<_> = block_on(decoded.collect());
    let mut events = Vec::new();
    let mut error = None;
    for item in items {
        assert!(error.is_none(), "an item followed the terminal error");
        match item {
            Ok(event) => events.push(event),
            Err(failure) => error = Some(failure),
        }
    }
    (events, error)
}

fn done(calls: Vec<ToolCall>, usage: Usage, reason: StopReason) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallsDone { calls },
        StreamEvent::Usage { usage },
        StreamEvent::Stop { reason },
    ]
}

const TEXT_TURN: [&str; 4] = [
    r#"{"model":"qwen3:8b","created_at":"2025-07-07T20:32:53.844124Z","message":{"role":"assistant","content":"","thinking":"The user greets"},"done":false}"#,
    r#"{"model":"qwen3:8b","created_at":"2025-07-07T20:32:53.9Z","message":{"role":"assistant","content":"Hél"},"done":false}"#,
    r#"{"model":"qwen3:8b","created_at":"2025-07-07T20:32:54.0Z","message":{"role":"assistant","content":"lo"},"done":false}"#,
    r#"{"model":"qwen3:8b","created_at":"2025-07-07T20:32:54.1Z","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","total_duration":4883583458,"load_duration":1334875,"prompt_eval_count":26,"prompt_eval_duration":342546000,"eval_count":282,"eval_duration":4535599000}"#,
];

#[test]
fn text_turn_decodes_over_any_network_split() {
    let wire = ndjson(&TEXT_TURN);
    let mut expected = vec![
        StreamEvent::ReasoningDelta {
            text: "The user greets".into(),
        },
        StreamEvent::TextDelta {
            text: "Hél".into()
        },
        StreamEvent::TextDelta { text: "lo".into() },
    ];
    expected.extend(done(
        Vec::new(),
        Usage {
            input_tokens: 26,
            output_tokens: 282,
            ..NO_USAGE
        },
        StopReason::EndTurn,
    ));
    // Split 1 cuts the multi-byte `é` and every record boundary.
    for split in [1, 7, wire.len()] {
        let (events, error) = run(&wire, split);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(events, expected, "split {split}");
    }
}

const TOOL_TURN: [&str; 2] = [
    r#"{"model":"llama3.2","created_at":"2025-07-07T20:32:53.844124Z","message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"get_weather","arguments":{"city":"Tokyo","unit": "c"}}}]},"done":false}"#,
    r#"{"model":"llama3.2","created_at":"2025-07-07T20:32:54.1Z","message":{"role":"assistant","content":""},"done_reason":"stop","done":true,"total_duration":1,"prompt_eval_count":169,"prompt_eval_cached_count":100,"eval_count":25}"#,
];

#[test]
fn tool_turn_keeps_raw_argument_bytes_and_promotes_a_stop_to_tool_use() {
    let wire = ndjson(&TOOL_TURN);
    let id = "ollama:0:get_weather".to_owned();
    let args = r#"{"city":"Tokyo","unit": "c"}"#;
    let mut expected = vec![
        StreamEvent::ToolCallStarted {
            id: id.clone(),
            name: "get_weather".into(),
        },
        StreamEvent::ToolArgsDelta {
            id: id.clone(),
            fragment: args.as_bytes().to_vec(),
        },
    ];
    expected.extend(done(
        vec![ToolCall {
            id,
            name: "get_weather".into(),
            args: ToolArgs::Parsed(raw(args)),
        }],
        Usage {
            input_tokens: 169,
            cached_input_tokens: 100,
            output_tokens: 25,
            ..NO_USAGE
        },
        StopReason::ToolUse,
    ));
    for split in [3, wire.len()] {
        let (events, error) = run(&wire, split);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(events, expected, "split {split}");
    }
}

#[test]
fn calls_take_ordinal_ids_and_tolerate_string_missing_and_unnamed_arguments() {
    let wire = ndjson(&[
        r#"{"message":{"role":"assistant","tool_calls":[{"function":{"name":"a","arguments":"{\"x\":1}"}},{"function":{"name":"b"}},{"function":{"arguments":{}}}]},"done":false}"#,
        r#"{"message":{"role":"assistant"},"done":true,"done_reason":"tool_calls","prompt_eval_count":1,"eval_count":2}"#,
    ]);
    let (events, error) = run(&wire, wire.len());
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events[events.len() - 3..],
        done(
            vec![
                ToolCall {
                    id: "ollama:0:a".into(),
                    name: "a".into(),
                    args: ToolArgs::Parsed(raw(r#"{"x":1}"#)),
                },
                ToolCall {
                    id: "ollama:1:b".into(),
                    name: "b".into(),
                    args: ToolArgs::Parsed(raw("{}")),
                },
                ToolCall {
                    id: "ollama:2:unknown_tool".into(),
                    name: "unknown_tool".into(),
                    args: ToolArgs::Parsed(raw("{}")),
                },
            ],
            Usage {
                input_tokens: 1,
                output_tokens: 2,
                ..NO_USAGE
            },
            StopReason::ToolUse,
        )
    );
}

#[test]
fn a_length_stop_truncates_every_call_and_reports_max_tokens() {
    let wire = ndjson(&[
        r#"{"message":{"role":"assistant","content":"partial","tool_calls":[{"function":{"name":"run","arguments":{"cmd":"ls"}}}]},"done":false}"#,
        r#"{"message":{"role":"assistant"},"done":true,"done_reason":"length","prompt_eval_count":5,"eval_count":9}"#,
    ]);
    let (events, error) = run(&wire, wire.len());
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events[events.len() - 3..],
        done(
            vec![ToolCall {
                id: "ollama:0:run".into(),
                name: "run".into(),
                args: ToolArgs::Truncated,
            }],
            Usage {
                input_tokens: 5,
                output_tokens: 9,
                ..NO_USAGE
            },
            StopReason::MaxTokens,
        )
    );
}

#[test]
fn an_unknown_done_reason_is_carried_and_cached_usage_never_exceeds_the_prompt() {
    let wire = ndjson(&[
        r#"{"message":{"role":"assistant","content":"hi"},"done":false}"#,
        r#"{"message":{"role":"assistant"},"done":true,"done_reason":"unload","prompt_eval_count":10,"prompt_eval_cached_count":50,"eval_count":1}"#,
    ]);
    let (events, error) = run(&wire, wire.len());
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events[events.len() - 3..],
        done(
            Vec::new(),
            Usage {
                input_tokens: 10,
                cached_input_tokens: 10,
                output_tokens: 1,
                ..NO_USAGE
            },
            StopReason::Other("unload".into()),
        )
    );
}

#[test]
fn a_done_record_without_counts_reports_zero_usage() {
    let wire = ndjson(&[r#"{"message":{"role":"assistant","content":"ok"},"done":true}"#]);
    let (events, error) = run(&wire, wire.len());
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events,
        [
            vec![StreamEvent::TextDelta { text: "ok".into() }],
            done(Vec::new(), NO_USAGE, StopReason::EndTurn),
        ]
        .concat()
    );
}

#[test]
fn an_empty_load_or_length_completion_is_an_error_not_a_stop() {
    let load = ndjson(&[
        r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"load"}"#,
    ]);
    let (events, error) = run(&load, load.len());
    assert!(events.is_empty(), "{events:?}");
    assert!(matches!(
        error,
        Some(ProviderError::Status { family: Family::Ollama, status: 200, ref message })
            if message == EMPTY_OLLAMA_LOAD_MESSAGE
    ));

    let length = ndjson(&[
        r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"length","prompt_eval_count":4096}"#,
    ]);
    let (events, error) = run(&length, length.len());
    assert!(events.is_empty(), "{events:?}");
    assert!(matches!(
        error,
        Some(ProviderError::Status { family: Family::Ollama, status: 200, ref message })
            if message == EMPTY_OLLAMA_LENGTH_MESSAGE
    ));
}

#[test]
fn end_of_input_before_a_done_record_is_a_cut() {
    let wire = ndjson(&TEXT_TURN[..3]);
    let (events, error) = run(&wire, 5);
    assert_eq!(events.len(), 3, "{events:?}");
    assert!(matches!(error, Some(ProviderError::StreamCut)), "{error:?}");
}

#[test]
fn the_final_record_decodes_without_its_newline_and_crlf_framing_is_accepted() {
    let wire = TEXT_TURN.join("\r\n");
    let (events, error) = run(&wire, 11);
    assert!(error.is_none(), "{error:?}");
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    ));
    assert_eq!(events.len(), 6, "{events:?}");
}

#[test]
fn blank_lines_and_unknown_members_are_ignored() {
    let wire = format!(
        "\n{}\n\n{}\n",
        r#"{"message":{"role":"assistant","content":"x","images":null,"new_member":{"a":1}},"done":false,"future":true}"#,
        r#"{"message":{"role":"assistant"},"done":true,"done_reason":"stop","metrics":{"k":1}}"#,
    );
    let (events, error) = run(&wire, wire.len());
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events,
        [
            vec![StreamEvent::TextDelta { text: "x".into() }],
            done(Vec::new(), NO_USAGE, StopReason::EndTurn),
        ]
        .concat()
    );
}

#[test]
fn an_error_record_and_a_malformed_line_end_the_stream_with_their_errors() {
    let wire = ndjson(&[
        r#"{"message":{"role":"assistant","content":"par"},"done":false}"#,
        r#"{"error":"model runner has unexpectedly stopped"}"#,
        r#"{"message":{"role":"assistant","content":"never"},"done":false}"#,
    ]);
    let (events, error) = run(&wire, wire.len());
    assert_eq!(events, [StreamEvent::TextDelta { text: "par".into() }]);
    assert!(matches!(
        error,
        Some(ProviderError::Status { family: Family::Ollama, status: 200, ref message })
            if message == "model runner has unexpectedly stopped"
    ));

    let (events, error) = run("this is not json\n", 4);
    assert_eq!(events, []);
    assert!(matches!(
        error,
        Some(ProviderError::Protocol {
            family: Family::Ollama,
            ..
        })
    ));
}

#[test]
fn the_decoder_emits_one_terminal_and_ignores_later_input() {
    let mut decoder = OllamaDecoder::new();
    let mut out = Vec::new();
    for record in &TEXT_TURN {
        assert!(decoder.feed(record, &mut out).is_ok());
    }
    let settled = out.len();
    assert_eq!(settled, 6);
    assert_eq!(
        decoder
            .feed(r#"{"message":{"content":"late"},"done":true}"#, &mut out)
            .unwrap(),
        Progress::Ended
    );
    assert!(decoder.end().is_ok());
    assert_eq!(
        out.len(),
        settled,
        "input after the terminal produced events"
    );
}

#[test]
fn an_unterminated_tail_past_the_record_bound_is_a_protocol_error() {
    let wire = "x".repeat(crate::sse::SSE_LINE_LIMIT + 1);
    let (events, error) = run(&wire, 64 << 10);
    assert_eq!(events, []);
    assert!(matches!(
        error,
        Some(ProviderError::Protocol {
            family: Family::Ollama,
            ref detail,
        }) if detail == "ollama record exceeds 1048576 bytes"
    ));
}

#[test]
fn one_big_chunk_of_short_records_decodes_without_a_bound_error() {
    let record = r#"{"message":{"role":"assistant","content":"x"},"done":false}"#;
    let count = 1_600_000 / record.len();
    let mut wire = format!("{record}\n").repeat(count);
    wire.push_str(r#"{"message":{"role":"assistant"},"done":true,"done_reason":"stop"}"#);
    // The whole 1.6 MiB arrives as one chunk, far past the per-record bound.
    let (events, error) = run(&wire, wire.len());
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        events.len(),
        count + 3,
        "one delta per record plus the terminal"
    );
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    ));
}

#[test]
fn cumulative_records_stop_at_the_stream_event_budget() {
    let mut decoder = OllamaDecoder::new();
    let data = r#"{"message":{"role":"assistant","content":"x"},"done":false}"#;
    let mut out = Vec::new();
    for _ in 0..crate::stream_budget::STREAM_EVENTS_LIMIT {
        assert!(decoder.feed(data, &mut out).is_ok());
        out.clear();
    }
    assert!(matches!(
        decoder.feed(data, &mut out),
        Err(ProviderError::Limit(crate::error::LimitError::StreamEvents))
    ));
}

#[test]
fn dropping_the_stream_before_its_terminal_drops_the_byte_source() {
    struct Flag(Arc<AtomicBool>);
    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = Flag(Arc::clone(&dropped));
    let first = format!("{}\n", TEXT_TURN[1]).into_bytes();
    // One record held in state, then a transfer that never yields again.
    // The flag lives in the byte source: it flips only when the source is
    // dropped.
    let source = stream::unfold((flag, Some(first)), |(flag, chunk)| async move {
        let chunk = chunk?;
        Some((chunk, (flag, None)))
    });
    let mut decoded = Box::pin(decode_stream(source, OllamaDecoder::new()));
    let first = block_on(decoded.next());
    assert!(matches!(first, Some(Ok(StreamEvent::TextDelta { .. }))));
    assert!(!dropped.load(Ordering::SeqCst));
    drop(decoded);
    assert!(
        dropped.load(Ordering::SeqCst),
        "the byte source outlived the stream"
    );
}

#[test]
fn the_full_path_runs_through_the_event_stream_guard() {
    let chunks = vec![ndjson(&TEXT_TURN).into_bytes()];
    let mut events = crate::stream::EventStream::new(
        decode_stream(stream::iter(chunks), OllamaDecoder::new()),
        || panic!("a clean Stop must not cancel"),
    );
    let seen: Vec<_> = block_on(async {
        let mut seen = Vec::new();
        while let Some(item) = events.next().await {
            seen.push(item.unwrap());
        }
        seen
    });
    assert_eq!(seen.len(), 6);
    assert!(matches!(seen.last(), Some(StreamEvent::Stop { .. })));
}

#[test]
fn tags_decode_prefers_model_over_name_and_ignores_unknown_members() {
    let body = br#"{"models":[
        {"name":"qwen3:8b","model":"qwen3:8b","modified_at":"2025-07-07T00:00:00Z","size":5225388164,
         "digest":"500a1f067a9f","details":{"parent_model":"","format":"gguf","family":"qwen3",
         "families":["qwen3"],"parameter_size":"8.2B","quantization_level":"Q4_K_M"}},
        {"name":"llama3.2:latest","details":{"family":"llama"}},
        {"model":"only-model"}
    ],"future":1}"#;
    let models = decode_tags(body).unwrap();
    let ids: Vec<_> = models.iter().map(OllamaModel::wire_id).collect();
    assert_eq!(
        ids,
        [
            Some("qwen3:8b"),
            Some("llama3.2:latest"),
            Some("only-model")
        ]
    );

    assert_eq!(decode_tags(br"{}").unwrap().len(), 0);
    assert!(matches!(
        decode_tags(b"<html>"),
        Err(ProviderError::Protocol {
            family: Family::Ollama,
            ..
        })
    ));
}

#[test]
fn request_body_ignores_text_tool_and_message_replay() {
    let assistant = |replay: fn() -> Option<RawJson>| ContextItem::Assistant {
        source: replay_source(),
        replay: replay(),
        parts: vec![
            AssistantPart::Text {
                text: "Hi.".into(),
                replay: replay(),
            },
            AssistantPart::ToolCall {
                call: CallId::new("call-9"),
                name: "read".into(),
                args: raw("{}"),
                replay: replay(),
            },
        ],
    };
    let payload = || Some(raw(r#"{"foreign":"REPLAY-MARKER"}"#));
    let plain = body_text(
        &request(Vec::new(), vec![assistant(|| None)]),
        &plan(None, None),
    );
    let laden = body_text(
        &request(Vec::new(), vec![assistant(payload)]),
        &plan(None, None),
    );
    assert_eq!(laden, plain);
    assert!(!laden.contains("REPLAY-MARKER"), "{laden}");
}
