use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn priced_usage() -> Usage {
    Usage {
        input_tokens: 2_000_000,
        cached_input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        reasoning_tokens: Some(500_000),
        cache_write_tokens: 100_000,
        cost_usd: None,
    }
}

fn price(input: f64, cached_input: f64, output: f64, reasoning: f64) -> ModelPrice {
    ModelPrice {
        input,
        cached_input,
        output,
        reasoning,
    }
}

#[test]
fn reported_configured_and_compiled_cost_precedence() {
    let configured = price(3.0, 3.0, 3.0, 3.0);
    let compiled = price(1.0, 1.0, 1.0, 1.0);
    let usage = priced_usage();
    assert_eq!(usage.cost_usd(None, None), None);
    assert_eq!(usage.cost_usd(None, Some(&compiled)), Some(3.5));
    assert_eq!(
        usage.cost_usd(Some(&configured), Some(&compiled)),
        Some(10.5)
    );
    assert_eq!(
        usage.cost_usd(Some(&price(-1.0, 0.0, 0.0, 0.0)), Some(&compiled)),
        None
    );
    let reported = Usage {
        cost_usd: Some(7.25),
        ..usage
    };
    assert_eq!(
        reported.cost_usd(Some(&configured), Some(&compiled)),
        Some(7.25)
    );
    assert_eq!(
        Usage {
            cost_usd: Some(0.0),
            ..usage
        }
        .cost_usd(None, None),
        Some(0.0)
    );
    assert_eq!(
        Usage {
            cost_usd: Some(f64::NAN),
            ..usage
        }
        .cost_usd(Some(&configured), None),
        None
    );
}

#[test]
fn usage_price_formula_does_not_double_count_cached_or_reasoning_tokens() {
    let usage = priced_usage();
    assert_eq!(price(1.0, 0.5, 2.0, 0.25).cost_usd(&usage), Some(3.625));
    assert_eq!(
        price(1.0, 1.0, 1.0, 1.0).cost_usd(&Usage {
            cache_write_tokens: 0,
            ..usage
        }),
        price(1.0, 1.0, 1.0, 1.0).cost_usd(&usage)
    );
    assert_eq!(
        price(1.0, 1.0, 1.0, 1.0).cost_usd(&Usage {
            cached_input_tokens: 2_000_001,
            ..usage
        }),
        None
    );
    assert_eq!(price(-1.0, 1.0, 1.0, 1.0).cost_usd(&usage), None);
    assert_eq!(price(f64::NAN, 1.0, 1.0, 1.0).cost_usd(&usage), None);
    assert_eq!(
        price(1e300, 1.0, 1.0, 1.0).cost_usd(&Usage {
            input_tokens: u64::MAX,
            cached_input_tokens: 0,
            ..usage
        }),
        None
    );
    let zero = Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    };
    assert_eq!(zero.cost_usd(None, None), None);
    assert_eq!(
        zero.cost_usd(Some(&price(0.0, 0.0, 0.0, 0.0)), None),
        Some(0.0)
    );
}

#[test]
fn uniform_rate_is_independent_of_cached_split() {
    let uniform = price(1_000_000.0, 1_000_000.0, 1_000_000.0, 1_000_000.0);
    for input in [0u16, 1, 2, 7, 10_000] {
        for cached in [0u16, 1, 2, 7, 10_000] {
            for output in [0u16, 1, 7] {
                let usage = Usage {
                    input_tokens: u64::from(input),
                    cached_input_tokens: u64::from(cached.min(input)),
                    output_tokens: u64::from(output),
                    reasoning_tokens: Some(7),
                    cache_write_tokens: 0,
                    cost_usd: None,
                };
                assert_eq!(
                    uniform.cost_usd(&usage),
                    Some(f64::from(input) + f64::from(output) + 7.0)
                );
            }
        }
    }
}

#[test]
fn model_info_context_window_roundtrips_unknown_zero_and_known() -> TestResult {
    let model_with_context = |context_window| ModelInfo {
        route: ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-5".into(),
        },
        name: "GPT-5".into(),
        caps: Caps {
            context_window,
            thinking: Vec::new().into_boxed_slice(),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        },
    };
    let known = model_with_context(Some(128_000));
    let unknown = model_with_context(None);
    let measured_zero = model_with_context(Some(0));
    let unknown_json = sonic_rs::to_string(&unknown)?;
    let zero_json = sonic_rs::to_string(&measured_zero)?;

    for model in [known, unknown, measured_zero] {
        let encoded = sonic_rs::to_string(&model)?;
        assert_eq!(sonic_rs::from_str::<ModelInfo>(&encoded)?, model);
    }
    assert_ne!(unknown_json, zero_json);
    Ok(())
}

#[test]
fn routes_validate_id_grammars_and_wire_names() -> TestResult {
    for (family, wire) in [
        (Family::Chat, "\"openai_chat\""),
        (Family::Responses, "\"openai_responses\""),
        (Family::Codex, "\"openai_codex\""),
        (Family::Anthropic, "\"anthropic\""),
    ] {
        assert_eq!(sonic_rs::to_string(&family)?, wire);
        assert_eq!(sonic_rs::from_str::<Family>(wire)?, family);
    }
    for id in ["a/b", "a-0/b.c_0-1"] {
        assert_eq!(ModelRoute::synthetic(id)?.id(), id);
    }
    for id in ["", "a", "/b", "a/", "A/b", "a/B", "a/b/c", "a.b/c", "a/b c"] {
        assert!(
            ModelRoute::synthetic(id).is_err(),
            "accepted invalid id {id}"
        );
        assert!(
            sonic_rs::from_str::<ModelRoute>(&format!(r#"{{"kind":"synthetic","id":"{id}"}}"#))
                .is_err()
        );
    }
    for id in ["dalgon/normal", "dalgon/eval-first", "dalgon/eval-only"] {
        assert_eq!(ModelRoute::harness(id)?.id(), id);
    }
    assert!(ModelRoute::harness("dalgon/other").is_err());
    assert!(sonic_rs::from_str::<ModelRoute>(r#"{"kind":"harness","id":"dalgon/other"}"#).is_err());
    let api = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-5".into(),
    };
    assert_eq!(api.id(), "gpt-5");
    assert_eq!(
        sonic_rs::to_string(&api)?,
        r#"{"kind":"api","family":"openai_chat","model":"gpt-5"}"#
    );
    assert_eq!(
        sonic_rs::from_str::<ModelRoute>(&sonic_rs::to_string(&api)?)?,
        api
    );
    Ok(())
}

#[test]
fn synthetic_chain_rejects_depth_before_cycle_scan() -> TestResult {
    let routes: Vec<ModelRoute> = ["a/one", "a/two", "a/three", "a/four", "a/five"]
        .into_iter()
        .map(ModelRoute::synthetic)
        .collect::<Result<_, _>>()?;
    assert!(check_synthetic_chain(&routes[..4]).is_ok());
    assert!(
        matches!(check_synthetic_chain(&routes), Err(InferFailure::SyntheticDepth { chain }) if chain == routes)
    );
    let repeated = vec![routes[0].clone(), routes[1].clone(), routes[0].clone()];
    assert!(
        matches!(check_synthetic_chain(&repeated), Err(InferFailure::SyntheticCycle { chain }) if chain == repeated)
    );
    let long_invalid = vec![
        routes[0].clone(),
        routes[1].clone(),
        routes[2].clone(),
        routes[3].clone(),
        routes[0].clone(),
    ];
    assert!(
        matches!(check_synthetic_chain(&long_invalid), Err(InferFailure::SyntheticDepth { chain }) if chain == long_invalid)
    );
    assert!(check_synthetic_chain(&[]).is_ok());
    Ok(())
}

#[test]
fn classified_failures_display_the_rendered_text_verbatim() {
    let text = "openai error 503: unavailable";
    let failures = [
        InferFailure::Overflow {
            code: "context_length_exceeded".into(),
            message: text.into(),
        },
        InferFailure::Retryable {
            hint: Some(Duration::from_secs(2)),
            message: text.into(),
        },
        InferFailure::Fatal {
            message: text.into(),
            fix: Some("Run dalgon login p1.".into()),
        },
    ];
    for failure in failures {
        assert_eq!(failure.to_string(), text);
    }
    assert_eq!(
        InferFailure::Cancelled.to_string(),
        "inference was cancelled"
    );
}

#[test]
fn native_tags_preserve_raw_args_and_nested_replay() -> TestResult {
    let arg_json = r#"{"b":2, "a":1e+02}"#;
    let event_json =
        format!(r#"{{"type":"tool_call","call":"c1","name":"run","args":{arg_json}}}"#);
    let event: StreamEvent = sonic_rs::from_str(&event_json)?;
    assert!(matches!(&event, StreamEvent::ToolCall { args, .. } if args.as_str() == arg_json));
    let encoded = sonic_rs::to_string(&event)?;
    assert!(
        encoded.contains(arg_json),
        "raw arguments changed: {encoded}"
    );
    assert_eq!(sonic_rs::from_str::<StreamEvent>(&encoded)?, event);

    let nested_json = format!(
        r#"{{"role":"assistant","source":{{"family":"openai_responses","model":"gpt-5"}},"parts":[{{"type":"thinking","text":"plan","replay":{arg_json}}},{{"type":"tool_call","call":"c1","name":"run","args":{arg_json}}}]}}"#
    );
    let context: ContextItem = sonic_rs::from_str(&nested_json)?;
    let encoded = sonic_rs::to_string(&context)?;
    assert_eq!(
        encoded.matches(arg_json).count(),
        2,
        "raw nested values changed: {encoded}"
    );
    assert_eq!(sonic_rs::from_str::<ContextItem>(&encoded)?, context);
    let thinking_at = encoded
        .find(r#""type":"thinking""#)
        .expect("thinking part was serialized");
    let tool_call_at = encoded
        .find(r#""type":"tool_call""#)
        .expect("tool-call part was serialized");
    assert!(thinking_at < tool_call_at, "assistant part order changed");

    let spec_json = format!(r#"{{"name":"run","description":"a tool","parameters":{arg_json}}}"#);
    let spec: ModelToolSpec = sonic_rs::from_str(&spec_json)?;
    assert!(sonic_rs::to_string(&spec)?.contains(arg_json));
    Ok(())
}

#[test]
fn stream_usage_and_stop_keep_their_tagged_newtype_wire() -> TestResult {
    let usage = priced_usage();
    let event = StreamEvent::Usage(usage);
    let encoded = sonic_rs::to_string(&event)?;
    assert!(encoded.contains(r#""type":"usage""#));
    assert!(encoded.contains(r#""input_tokens":2000000"#));
    assert_eq!(sonic_rs::from_str::<StreamEvent>(&encoded)?, event);
    for reason in [
        Stop::EndTurn,
        Stop::Length,
        Stop::Filter,
        Stop::MaxSteps,
        Stop::Cancelled,
        Stop::Failed,
    ] {
        let event = StreamEvent::Stop(reason);
        let encoded = sonic_rs::to_string(&event)?;
        assert!(encoded.contains(r#""type":"stop""#), "{encoded}");
        assert_eq!(sonic_rs::from_str::<StreamEvent>(&encoded)?, event);
    }
    assert_eq!(
        sonic_rs::to_string(&StreamEvent::Stop(Stop::EndTurn))?,
        r#"{"type":"stop","end_turn":null}"#
    );
    Ok(())
}

#[test]
fn assistant_context_requires_a_valid_replay_source() {
    for text in [
        r#"{"role":"assistant","parts":[]}"#,
        r#"{"role":"assistant","source":{"family":"alien","model":"gpt-5"},"parts":[]}"#,
        r#"{"role":"assistant","source":{"family":"openai_responses"},"parts":[]}"#,
        r#"{"role":"assistant","source":{"family":"openai_responses","model":""},"parts":[]}"#,
    ] {
        assert!(
            sonic_rs::from_str::<ContextItem>(text).is_err(),
            "accepted {text}"
        );
    }
}

#[test]
fn malformed_model_discriminators_and_stop_reasons_fail() {
    for text in [
        r#"{"type":"tool_call","call":"c1","name":"run","args":{},"type":"delta"}"#,
        r#"{"type":"alien"}"#,
        r#"{"call":"c1","name":"run","args":{}}"#,
        r#"{"type":"stop"}"#,
        r#"{"type":"stop","end_turn":1}"#,
        r#"{"type":"stop","end_turn":null,"length":null}"#,
        r#"{"type":"delta"}"#,
        r#"{"type":"thinking_replay"}"#,
        r#"{"type":"thinking_replay","payload":{},"payload":{}}"#,
        r#"{"type":"thinking_replay","payload":{"a":}}"#,
        r#"{"type":"thinking_replay","type":"delta","payload":{}}"#,
        r#"{"type":"thinking-replay","payload":{}}"#,
    ] {
        assert!(
            sonic_rs::from_str::<StreamEvent>(text).is_err(),
            "accepted {text}"
        );
    }
    for text in [
        r#"{"role":"alien"}"#,
        r#"{"parts":[]}"#,
        r#"{"role":"user","role":"user","parts":[]}"#,
    ] {
        assert!(
            sonic_rs::from_str::<ContextItem>(text).is_err(),
            "accepted {text}"
        );
    }
    for text in [
        r#"{"type":"alien"}"#,
        r#"{"text":"hello"}"#,
        r#"{"type":"thinking","text":"hello","replay":{broken}}"#,
    ] {
        assert!(
            sonic_rs::from_str::<AssistantPart>(text).is_err(),
            "accepted {text}"
        );
    }
    for text in [
        r#"{"kind":"alien"}"#,
        r#"{"id":"a/b"}"#,
        r#"{"kind":"synthetic","kind":"synthetic","id":"a/b"}"#,
    ] {
        assert!(
            sonic_rs::from_str::<ModelRoute>(text).is_err(),
            "accepted {text}"
        );
    }
}

#[test]
fn request_and_inference_round_trip_through_nested_carriers() -> TestResult {
    let request = ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::synthetic("pool/fast")?,
        system: Arc::from("be useful"),
        tools: Arc::from([ModelToolSpec {
            name: "run".into(),
            description: "executes".into(),
            parameters: RawJson::parse(r#"{"z":1e+02, "a":2}"#)?,
            grammar: None,
        }]),
        context: Arc::from([ContextItem::Assistant {
            source: ReplaySource {
                family: Family::Responses,
                model: "gpt-5".into(),
            },
            parts: vec![AssistantPart::Thinking {
                text: "plan".into(),
                replay: Some(RawJson::parse(r#"{"z":1e+02, "a":2}"#)?),
            }],
        }]),
        params: RequestParams {
            thinking: ThinkingLevel::Xhigh,
            effort: Some("high".into()),
            temperature: Some(0.5),
            max_output_tokens: None,
        },
        cache_key: Some("scope".into()),
    };
    let encoded = sonic_rs::to_string(&request)?;
    assert_eq!(encoded.matches(r#"{"z":1e+02, "a":2}"#).count(), 2);
    assert_eq!(sonic_rs::from_str::<ModelRequest>(&encoded)?, request);
    let inference = Inference {
        events: vec![
            StreamEvent::Delta {
                channel: StreamChannel::ToolArgs { tool: "run".into() },
                text: "x".into(),
            },
            StreamEvent::Stop(Stop::EndTurn),
        ],
    };
    assert_eq!(
        sonic_rs::from_str::<Inference>(&sonic_rs::to_string(&inference)?)?,
        inference
    );
    Ok(())
}

#[test]
fn thinking_replay_event_preserves_raw_payload_after_delta() -> TestResult {
    let payload = r#"{"z":1e+02, "a":[2.50,"x"]}"#;
    let text = format!(
        r#"{{"events":[{{"type":"delta","channel":{{"type":"thinking"}},"text":"plan"}},{{"payload":{payload},"type":"thinking_replay"}}]}}"#
    );
    let inference: Inference = sonic_rs::from_str(&text)?;
    assert_eq!(
        inference.events,
        vec![
            StreamEvent::Delta {
                channel: StreamChannel::Thinking,
                text: "plan".into(),
            },
            StreamEvent::ThinkingReplay {
                payload: RawJson::parse(payload)?,
            },
        ]
    );
    let encoded = sonic_rs::to_string(&inference)?;
    assert!(
        encoded.contains(&format!(
            r#"{{"type":"thinking_replay","payload":{payload}}}"#
        )),
        "replay payload changed: {encoded}"
    );
    assert_eq!(sonic_rs::from_str::<Inference>(&encoded)?, inference);
    Ok(())
}
