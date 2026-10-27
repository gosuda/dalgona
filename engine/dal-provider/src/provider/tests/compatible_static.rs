//! Wire proofs of the compatible presets that need nothing beyond the
//! selected Chat backend.
//!
//! Every case decodes a strict configuration, builds the real
//! [`ProviderSet`], and runs the real request lifecycle against a loopback
//! server, so the environment key, the header set, the model list, the body,
//! and the stream decode are the ones a host gets. Nothing reaches a vendor.

use dal_core::{AssistantPart, CallId, ReplaySource};
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use super::*;
use crate::stream::{StopReason, ToolArgs};

/// The reasoning spelling Cerebras streams (`reasoning`), then text, then the
/// final chunk with its usage.
fn cerebras_reasoning_reply() -> String {
    concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"Weigh it. \"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Done.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":21,\"completion_tokens\":9,\"prompt_tokens_details\":{\"cached_tokens\":16},\"completion_tokens_details\":{\"reasoning_tokens\":4}}}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned()
}

/// One tool call whose arguments `{"path":"src/main.rs"}` arrive in two
/// fragments.
fn split_tool_call_reply(id: &str, name: &str) -> String {
    format!(
        concat!(
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"{id}\",\"type\":\"function\",\"function\":{{\"name\":\"{name}\",\"arguments\":\"{{\\\"path\\\":\"}}}}]}},\"finish_reason\":null}}]}}\n\n",
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":\"\\\"src/main.rs\\\"}}\"}}}}]}},\"finish_reason\":null}}]}}\n\n",
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\n",
            "data: [DONE]\n\n",
        ),
        id = id,
        name = name,
    )
}

fn listing(body: &str) -> Reply {
    Reply {
        status: 200,
        content_type: "application/json",
        body: body.as_bytes().to_vec(),
    }
}

fn cerebras_set(dir: &TestDir, base: &str, key: &str) -> Result<ProviderSet, Box<dyn Error>> {
    preset_set(
        dir,
        &format!("[providers.cerebras]\nbase_url = '{base}'\n"),
        &[("CEREBRAS_API_KEY", key)],
    )
}

/// The request head of a captured request, lowercased.
fn head_of(captured: &[u8]) -> String {
    let end = captured
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a request head ends");
    String::from_utf8_lossy(&captured[..end]).to_ascii_lowercase()
}

fn body_json(captured: &[u8]) -> sonic_rs::Value {
    sonic_rs::from_slice(request_body(captured)).expect("the request body is JSON")
}

/// Runs one turn on `route` and returns its events.
async fn run_turn(
    set: &ProviderSet,
    route: ResolvedModel,
    ask: &ModelRequest,
) -> Result<Vec<StreamEvent>, Box<dyn Error>> {
    let provider = set.provider(route)?;
    let stream = provider
        .open(
            SessionId::new_v7(),
            ask,
            &[],
            notice_sink(),
            &CancellationToken::new(),
        )
        .await?;
    Ok(collect(stream).await?)
}

/// The route a model listed by the live catalog resolves to.
fn listed_route(provider: &str, entry: CatalogEntry) -> ResolvedModel {
    ResolvedModel {
        provider: provider.into(),
        route: ModelRoute::Api {
            family: Family::Chat,
            model: entry.id.clone(),
            provider: None,
        },
        entry,
    }
}

/// The reasoning, text, usage, and stop of [`cerebras_reasoning_reply`].
fn assert_reasoning_turn(events: &[StreamEvent]) {
    assert!(
        events.iter().any(
            |event| matches!(event, StreamEvent::ReasoningDelta { text } if text == "Weigh it. ")
        ),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Done.")),
        "{events:?}"
    );
    let usage = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Usage { usage } => Some(usage),
            _ => None,
        })
        .expect("the stream reports usage");
    assert_eq!(usage.input_tokens, 21);
    assert_eq!(usage.cached_input_tokens, 16);
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(usage.reasoning_tokens, Some(4));
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    ));
}

/// The header set of one Cerebras request: a bearer key, nothing foreign.
fn assert_bearer_head(head: &str, key: &str) {
    assert!(
        head.contains(&format!("authorization: bearer {key}")),
        "{head}"
    );
    for forbidden in ["\nx-api-key:", "\napi-key:", "\nanthropic-version:"] {
        assert!(!head.contains(forbidden), "{forbidden:?} is in {head}");
    }
}

/// The body members of a Cerebras turn at thinking `high`.
fn assert_effort_body(body: &sonic_rs::Value) {
    assert_eq!(
        body.get("model").and_then(|value| value.as_str()),
        Some("gpt-oss-120b")
    );
    // The system text keeps the `system` role: this host takes no
    // `developer` role.
    assert_eq!(
        body.get("messages")
            .and_then(|messages| messages.get(0usize))
            .and_then(|message| message.get("role"))
            .and_then(|role| role.as_str()),
        Some("system")
    );
    // The effort is the OpenAI `reasoning_effort` member and nothing else:
    // no `thinking` object, no Qwen-native switch, no `store`, no token cap.
    assert_eq!(
        body.get("reasoning_effort")
            .and_then(|value| value.as_str()),
        Some("high")
    );
    for absent in [
        "thinking",
        "enable_thinking",
        "chat_template_kwargs",
        "reasoning",
        "store",
        "max_tokens",
        "max_completion_tokens",
    ] {
        assert!(body.get(absent).is_none(), "{absent} is in the body");
    }
}

#[tokio::test]
async fn a_listed_cerebras_model_streams_chat_with_its_key_and_the_effort_as_reasoning_effort()
-> TestResult {
    let dir = TestDir::new()?;
    let (base, mut captured) = server(
        "/v1",
        vec![
            listing(
                r#"{"object":"list","data":[{"id":"gpt-oss-120b","object":"model","created":0,"owned_by":"Cerebras"}]}"#,
            ),
            stream_reply(cerebras_reasoning_reply()),
        ],
    )
    .await?;
    let set = cerebras_set(&dir, &base, "csk-secret")?;

    // The model list is the catalog: a listed id with no vendor facts keeps
    // its limits unknown.
    let fetch = set.load_models("cerebras").await?;
    assert_eq!(fetch.source, CatalogSource::Live, "{:?}", fetch.live_error);
    let entry = fetch
        .entries
        .into_iter()
        .find(|entry| &*entry.id == "gpt-oss-120b")
        .expect("the listed model is in the catalog");
    assert_eq!(entry.context_window, None);
    assert_eq!(entry.max_output, None);

    let mut ask = request(Family::Chat, "gpt-oss-120b", vec![user_text("Weigh it.")]);
    ask.params.thinking = ThinkingLevel::High;
    let events = run_turn(&set, listed_route("cerebras", entry), &ask).await?;
    assert_reasoning_turn(&events);

    let requests = captured.join_next().await.expect("server completes")??;
    assert_eq!(requests.len(), 2);
    let list_head = head_of(&requests[0]);
    assert!(list_head.starts_with("get /v1/models "), "{list_head}");
    assert_bearer_head(&list_head, "csk-secret");
    let head = head_of(&requests[1]);
    assert!(head.starts_with("post /v1/chat/completions "), "{head}");
    assert!(head.contains("accept: text/event-stream"), "{head}");
    assert_bearer_head(&head, "csk-secret");
    assert_effort_body(&body_json(&requests[1]));
    Ok(())
}

const SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","format":"uri-reference"}},"required":["path"],"additionalProperties":false}"#;
const CALL: &str = "call_a1B2c3";

fn read_file_tools() -> Result<Arc<[ModelToolSpec]>, Box<dyn Error>> {
    Ok(Arc::from([ModelToolSpec {
        name: "read_file".into(),
        description: "Reads one file.".into(),
        parameters: RawJson::parse(SCHEMA)?,
        grammar: None,
        defer_loading: false,
    }]))
}

/// The arguments of the one call [`split_tool_call_reply`] streams.
fn assembled_call_args(events: &[StreamEvent]) -> Result<RawJson, Box<dyn Error>> {
    let calls = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::ToolCallsDone { calls } => Some(calls),
            _ => None,
        })
        .ok_or_else(|| io::Error::other("the stream ended no calls"))?;
    assert_eq!(calls.len(), 1);
    assert_eq!(
        (calls[0].id.as_str(), calls[0].name.as_str()),
        (CALL, "read_file")
    );
    let ToolArgs::Parsed(args) = &calls[0].args else {
        return Err(
            io::Error::other(format!("arguments did not assemble: {:?}", calls[0].args)).into(),
        );
    };
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::ToolUse
        })
    ));
    Ok(args.clone())
}

/// The turn that answers the streamed call by its id.
fn answered_call_turn(args: RawJson) -> Result<ModelRequest, Box<dyn Error>> {
    let mut ask = request(
        Family::Chat,
        "gpt-oss-120b",
        vec![
            user_text("Open main."),
            ContextItem::Assistant {
                source: ReplaySource {
                    family: Family::Chat,
                    model: "gpt-oss-120b".into(),
                },
                replay: None,
                parts: vec![AssistantPart::ToolCall {
                    call: CallId::new(CALL),
                    name: "read_file".into(),
                    args,
                    replay: None,
                }],
            },
            ContextItem::ToolResult {
                call: CallId::new(CALL),
                name: "read_file".into(),
                is_error: false,
                parts: vec![Part::Text {
                    text: "fn main() {}".into(),
                }],
            },
        ],
    );
    ask.tools = read_file_tools()?;
    Ok(ask)
}

/// The replayed call and its result in the second request body.
fn assert_call_and_result_pair(body: &sonic_rs::Value) {
    let messages = body
        .get("messages")
        .and_then(|messages| messages.as_array())
        .expect("messages is an array");
    let by_role = |role: &str| {
        messages
            .iter()
            .find(|message| message.get("role").and_then(|value| value.as_str()) == Some(role))
            .unwrap_or_else(|| panic!("no {role} message in {body:?}"))
    };
    let call = by_role("assistant")
        .get("tool_calls")
        .and_then(|calls| calls.get(0usize))
        .expect("the call is replayed");
    assert_eq!(call.get("id").and_then(|id| id.as_str()), Some(CALL));
    assert_eq!(
        call.get("function")
            .and_then(|function| function.get("arguments"))
            .and_then(|arguments| arguments.as_str()),
        Some(r#"{"path":"src/main.rs"}"#)
    );
    let result = by_role("tool");
    assert_eq!(
        result.get("tool_call_id").and_then(|id| id.as_str()),
        Some(CALL)
    );
    assert_eq!(
        result.get("content").and_then(|content| content.as_str()),
        Some("fn main() {}")
    );
}

#[tokio::test]
async fn cerebras_tool_calls_keep_ids_arguments_and_schema_bytes_and_never_carry_strict()
-> TestResult {
    let dir = TestDir::new()?;
    let (base, mut captured) = server(
        "/v1",
        vec![
            stream_reply(split_tool_call_reply(CALL, "read_file")),
            stream_reply(cerebras_reasoning_reply()),
        ],
    )
    .await?;
    let set = cerebras_set(&dir, &base, "csk-secret")?;
    let route = || resolved("cerebras", Family::Chat, "gpt-oss-120b");

    let mut first = request(Family::Chat, "gpt-oss-120b", vec![user_text("Open main.")]);
    first.tools = read_file_tools()?;
    let events = run_turn(&set, route(), &first).await?;
    let args = assembled_call_args(&events)?;
    assert_eq!(args.as_str(), r#"{"path":"src/main.rs"}"#);
    run_turn(&set, route(), &answered_call_turn(args)?).await?;

    let requests = captured.join_next().await.expect("server completes")??;
    assert_eq!(requests.len(), 2);
    for (index, wire) in requests.iter().enumerate() {
        let text = String::from_utf8_lossy(request_body(wire));
        // The schema travels byte for byte, and no tool is marked strict.
        assert!(
            text.contains(&format!(r#""parameters":{SCHEMA}"#)),
            "request {index}: {text}"
        );
        assert!(!text.contains("strict"), "request {index}: {text}");
    }
    assert_call_and_result_pair(&body_json(&requests[1]));
    Ok(())
}

/// `id` refuses to open without its own key: a typed error with the login
/// fix, and a typed model list that asked nothing.
async fn assert_no_key(set: &ProviderSet, id: &str) -> TestResult {
    let Err(error) = set.provider(resolved(id, Family::Chat, "preset-model")) else {
        return Err(io::Error::other(format!("{id} resolved without its own key")).into());
    };
    assert!(
        matches!(&error, ProviderError::NoCredentials { provider } if provider == id),
        "{id}: {error:?}"
    );
    assert_eq!(error.fix(), Some(format!("Run dalgon login {id}.")));
    let fetch = set.load_models(id).await?;
    assert_eq!(fetch.source, CatalogSource::Typed, "{id}");
    assert!(
        matches!(&fetch.live_error, Some(ProviderError::NoCredentials { provider }) if provider == id),
        "{id}: {:?}",
        fetch.live_error
    );
    Ok(())
}

#[tokio::test]
async fn cerebras_reads_only_its_own_key_variable() -> TestResult {
    let dir = TestDir::new()?;
    // Other vendors' keys are set and the Cerebras variable is empty, which
    // counts as unset: no key leaves for this host.
    let set = preset_set(
        &dir,
        "",
        &[
            ("OPENAI_API_KEY", "openai-secret"),
            ("GROQ_API_KEY", "groq-secret"),
            ("MOONSHOT_API_KEY", "moonshot-secret"),
            ("XIAOMI_API_KEY", "xiaomi-secret"),
            ("CEREBRAS_API_KEY", ""),
        ],
    )?;
    assert_no_key(&set, "cerebras").await
}

#[tokio::test]
async fn the_new_rows_and_their_origin_neighbours_never_share_a_key() -> TestResult {
    let dir = TestDir::new()?;
    // Each pair names a row and a variable that another origin owns: the
    // variable is set and must not key the row. `moonshotai-cn` takes
    // neither the `moonshot` row's variables nor `KIMI_API_KEY`.
    for (id, foreign) in [
        ("xiaomi", "XIAOMI_TOKEN_PLAN_CN_API_KEY"),
        ("xiaomi-token-plan-cn", "XIAOMI_API_KEY"),
        ("moonshotai-cn", "MOONSHOT_API_KEY"),
        ("moonshotai-cn", "KIMI_API_KEY"),
        ("moonshot", "MOONSHOTAI_CN_API_KEY"),
    ] {
        let set = preset_set(&dir, "", &[(foreign, "foreign-secret")])?;
        assert_no_key(&set, id).await?;
    }
    Ok(())
}

const REASONING: &str = "Weigh it.";

/// A history with one tool turn of `model`: its reasoning, the call, and the
/// result, then the next user message.
fn tool_history(model: &str) -> Result<Vec<ContextItem>, Box<dyn Error>> {
    Ok(vec![
        user_text("Open main."),
        ContextItem::Assistant {
            source: ReplaySource {
                family: Family::Chat,
                model: model.into(),
            },
            replay: None,
            parts: vec![
                AssistantPart::Thinking {
                    text: REASONING.into(),
                    replay: None,
                },
                AssistantPart::ToolCall {
                    call: CallId::new(CALL),
                    name: "read_file".into(),
                    args: RawJson::parse(r#"{"path":"src/main.rs"}"#)?,
                    replay: None,
                },
            ],
        },
        ContextItem::ToolResult {
            call: CallId::new(CALL),
            name: "read_file".into(),
            is_error: false,
            parts: vec![Part::Text {
                text: "fn main() {}".into(),
            }],
        },
        user_text("Once more."),
    ])
}

fn models_reply(model: &str) -> Reply {
    listing(&format!(
        r#"{{"object":"list","data":[{{"id":"{model}","object":"model","created":0,"owned_by":"test"}}]}}"#
    ))
}

/// Takes `model` from the model list of the preset `provider` (configured
/// with `extra` lines under its table), runs each of `turns` through the real
/// lifecycle, and returns the listed entry and every captured request: the
/// model list first, then one per turn.
async fn run_listed(
    provider: &str,
    extra: &str,
    env: &[(&str, &str)],
    model: &str,
    turns: &[ModelRequest],
) -> Result<(CatalogEntry, Vec<Vec<u8>>), Box<dyn Error>> {
    let dir = TestDir::new()?;
    let mut replies = vec![models_reply(model)];
    replies.extend(turns.iter().map(|_| stream_reply(chat_reply())));
    let (base, mut captured) = server("/v1", replies).await?;
    let toml = format!("[providers.{provider}]\nbase_url = '{base}'\n{extra}");
    let set = preset_set(&dir, &toml, env)?;
    let fetch = set.load_models(provider).await?;
    assert_eq!(fetch.source, CatalogSource::Live, "{:?}", fetch.live_error);
    let entry = fetch
        .entries
        .into_iter()
        .find(|entry| &*entry.id == model)
        .expect("the listed model is in the catalog");
    for ask in turns {
        run_turn(&set, listed_route(provider, entry.clone()), ask).await?;
    }
    let requests = captured.join_next().await.expect("server completes")??;
    assert_eq!(requests.len(), turns.len() + 1);
    Ok((entry, requests))
}

/// The `reasoning_content` of the first assistant message of a body.
fn replayed_reasoning(body: &sonic_rs::Value) -> Option<String> {
    body.get("messages")
        .and_then(|messages| messages.as_array())?
        .iter()
        .find(|message| message.get("role").and_then(|role| role.as_str()) == Some("assistant"))?
        .get("reasoning_content")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

fn thinking_type(body: &sonic_rs::Value) -> Option<&str> {
    body.get("thinking")
        .and_then(|thinking| thinking.get("type"))
        .and_then(|kind| kind.as_str())
}

/// The model list and the chat request of a run carry `key` and no other.
fn assert_signed_with(requests: &[Vec<u8>], key: &str, foreign: &[&str]) {
    let list = head_of(&requests[0]);
    let chat = head_of(&requests[1]);
    assert!(list.starts_with("get /v1/models "), "{list}");
    assert!(chat.starts_with("post /v1/chat/completions "), "{chat}");
    for head in [&list, &chat] {
        assert_bearer_head(head, key);
        for other in foreign {
            assert!(!head.contains(other), "{other} is in {head}");
        }
    }
}

#[tokio::test]
async fn xiaomi_signs_with_its_own_key_and_replays_mimo_reasoning_on_a_tool_turn() -> TestResult {
    let model = "mimo-v2.5";
    let mut ask = request(Family::Chat, model, tool_history(model)?);
    ask.params.thinking = ThinkingLevel::High;
    ask.tools = read_file_tools()?;
    let env = [
        ("XIAOMI_API_KEY", "sk-secret"),
        ("XIAOMI_TOKEN_PLAN_CN_API_KEY", "tp-secret"),
        ("MOONSHOT_API_KEY", "moonshot-secret"),
    ];
    let (entry, requests) = run_listed("xiaomi", "", &env, model, &[ask]).await?;
    // The list carries no limits, so none are invented.
    assert_eq!((entry.context_window, entry.max_output), (None, None));
    assert_signed_with(&requests, "sk-secret", &["tp-secret", "moonshot-secret"]);

    let body = body_json(&requests[1]);
    // `mimo-v2.5` is always on: the `thinking` object rides with the effort.
    assert_eq!(thinking_type(&body), Some("enabled"), "{body:?}");
    assert_eq!(
        body.get("reasoning_effort")
            .and_then(|value| value.as_str()),
        Some("high")
    );
    // The reasoning of the model's own tool turn travels back with the call.
    assert_call_and_result_pair(&body);
    assert_eq!(replayed_reasoning(&body).as_deref(), Some(REASONING));
    // MiMo takes the schema as written; only Moonshot lowers it.
    let text = String::from_utf8_lossy(request_body(&requests[1])).into_owned();
    assert!(
        text.contains(&format!(r#""parameters":{SCHEMA}"#)),
        "{text}"
    );
    Ok(())
}

#[tokio::test]
async fn moonshotai_cn_sends_the_kimi_body_under_its_own_row_and_key() -> TestResult {
    let model = "kimi-k3";
    let mut ask = request(Family::Chat, model, tool_history(model)?);
    ask.params.thinking = ThinkingLevel::High;
    ask.params.max_output_tokens = Some(2_048);
    ask.tools = read_file_tools()?;
    // The loopback base is not a Moonshot host: the row id alone selects the
    // Kimi body.
    let env = [
        ("MOONSHOTAI_CN_API_KEY", "cn-secret"),
        ("MOONSHOT_API_KEY", "moonshot-secret"),
        ("KIMI_API_KEY", "kimi-secret"),
    ];
    let (entry, requests) = run_listed("moonshotai-cn", "", &env, model, &[ask]).await?;
    assert_eq!((entry.context_window, entry.max_output), (None, None));
    assert_signed_with(&requests, "cn-secret", &["moonshot-secret", "kimi-secret"]);

    let body = body_json(&requests[1]);
    // The legacy cap field, the effort K3 takes, and the reasoning replay.
    assert_eq!(
        body.get("max_tokens")
            .and_then(sonic_rs::JsonValueTrait::as_u64),
        Some(2_048),
        "{body:?}"
    );
    assert!(body.get("max_completion_tokens").is_none(), "{body:?}");
    assert_eq!(
        body.get("reasoning_effort")
            .and_then(|value| value.as_str()),
        Some("high")
    );
    assert!(body.get("thinking").is_none(), "{body:?}");
    assert_call_and_result_pair(&body);
    assert_eq!(replayed_reasoning(&body).as_deref(), Some(REASONING));
    // Moonshot's schema subset: the `format` annotation is gone, the rest stays.
    let parameters = body
        .get("tools")
        .and_then(|tools| tools.get(0usize))
        .and_then(|tool| tool.get("function"))
        .and_then(|function| function.get("parameters"))
        .expect("the tool is advertised");
    let path = parameters
        .get("properties")
        .and_then(|properties| properties.get("path"))
        .expect("the path property survives schema lowering");
    assert_eq!(
        path.get("type").and_then(|value| value.as_str()),
        Some("string")
    );
    assert!(path.get("format").is_none(), "{parameters:?}");
    Ok(())
}

#[tokio::test]
async fn moonshotai_cn_takes_the_moonshot_key_only_when_key_env_names_it() -> TestResult {
    let model = "kimi-k2.6";
    let ask = || request(Family::Chat, model, vec![user_text("Say hello.")]);
    let env = [
        ("MOONSHOTAI_CN_API_KEY", "cn-secret"),
        ("MOONSHOT_API_KEY", "moonshot-secret"),
    ];
    // Both variables are set: the row's own wins.
    let (_, own) = run_listed("moonshotai-cn", "", &env, model, &[ask()]).await?;
    assert_signed_with(&own, "cn-secret", &["moonshot-secret"]);
    // The account opts in to its one key for both origins.
    let opted = "key_env = 'MOONSHOT_API_KEY'\n";
    let (_, shared) = run_listed("moonshotai-cn", opted, &env, model, &[ask()]).await?;
    assert_signed_with(&shared, "moonshot-secret", &["cn-secret"]);
    Ok(())
}
