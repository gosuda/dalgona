//! Wire proofs of the Chat facts the Cerebras, Moonshot, and Xiaomi hosts
//! need beyond the shared codec: the requested output cap, the Cerebras
//! `gpt-oss` effort ladder and stream options, the `MiMo` and Kimi thinking
//! switches, the reasoning replay of assistant tool turns, and the Moonshot
//! tool-schema subset.
//!
//! Every case decodes a strict configuration, builds the real
//! [`ProviderSet`], takes the model from the host's model list, and runs the
//! real request lifecycle against a loopback server, so the catalog row, the
//! thinking plan, and the body are the ones a host gets. Nothing reaches a
//! vendor.
//!
//! Only facts every consulted source agrees on are asserted: where the pinned
//! references disagree (the replay of text-only assistant turns, the `MiMo`
//! `reasoning_effort`), the case leaves the member unconstrained or asserts
//! the officially documented side, and the report names the conflict.

use dal_core::{AssistantPart, CallId, ReplaySource};
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use super::*;

/// A host a case runs against: its provider id and its key variable.
struct Host {
    provider: &'static str,
    key_env: &'static str,
}

const CEREBRAS: Host = Host {
    provider: "cerebras",
    key_env: "CEREBRAS_API_KEY",
};
const MOONSHOT: Host = Host {
    provider: "moonshot",
    key_env: "MOONSHOT_API_KEY",
};
/// The Xiaomi `MiMo` rows share one dialect; the China token plan stands for
/// them here.
const MIMO: Host = Host {
    provider: "xiaomi-token-plan-cn",
    key_env: "XIAOMI_TOKEN_PLAN_CN_API_KEY",
};
/// A host whose pinned output cap field is the legacy `max_tokens`.
const DEEPSEEK: Host = Host {
    provider: "deepseek",
    key_env: "DEEPSEEK_API_KEY",
};
/// An ordinary Chat row with no pinned difference: the reference cap field.
const GROQ: Host = Host {
    provider: "groq",
    key_env: "GROQ_API_KEY",
};

const CALL: &str = "call_a1B2c3";
const CALL_WITHOUT_REASONING: &str = "call_d4E5f6";
const CALL_OF_ANOTHER_MODEL: &str = "call_g7H8i9";
const REASONING: &str = "Weigh it.";

fn text_reply() -> String {
    concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Done.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned()
}

fn listing(model: &str) -> Reply {
    Reply {
        status: 200,
        content_type: "application/json",
        body: format!(
            r#"{{"object":"list","data":[{{"id":"{model}","object":"model","created":0,"owned_by":"test"}}]}}"#
        )
        .into_bytes(),
    }
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

/// Takes `model` from the host's model list, runs each of `turns` against
/// the real lifecycle, and returns the JSON body of every chat request.
async fn chat_bodies(
    host: &Host,
    model: &str,
    turns: Vec<ModelRequest>,
) -> Result<Vec<sonic_rs::Value>, Box<dyn Error>> {
    let dir = TestDir::new()?;
    let mut replies = vec![listing(model)];
    replies.extend(turns.iter().map(|_| stream_reply(text_reply())));
    let (base, mut captured) = server("/v1", replies).await?;
    let set = preset_set(
        &dir,
        &format!("[providers.{}]\nbase_url = '{base}'\n", host.provider),
        &[(host.key_env, "key-secret")],
    )?;
    let fetch = set.load_models(host.provider).await?;
    assert_eq!(fetch.source, CatalogSource::Live, "{:?}", fetch.live_error);
    let entry = fetch
        .entries
        .into_iter()
        .find(|entry| &*entry.id == model)
        .expect("the listed model is in the catalog");
    for ask in &turns {
        let provider = set.provider(listed_route(host.provider, entry.clone()))?;
        let stream = provider
            .open(
                SessionId::new_v7(),
                ask,
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?;
        collect(stream).await?;
    }
    let requests = captured.join_next().await.expect("server completes")??;
    assert_eq!(requests.len(), turns.len() + 1);
    Ok(requests[1..]
        .iter()
        .map(|wire| parse_body(request_body(wire)))
        .collect())
}

/// One request body with every number's exact text kept, so a case can
/// assert the digits the host receives (a plain `from_slice::<Value>` would
/// turn `18446744073709551617` into a float and re-spell `1e3`).
fn parse_body(wire: &[u8]) -> sonic_rs::Value {
    let mut deserializer = sonic_rs::Deserializer::from_slice(wire).use_rawnumber();
    let body = deserializer
        .deserialize()
        .expect("the request body is JSON");
    deserializer
        .end()
        .expect("the request body is one JSON value");
    body
}

fn turn(model: &str, level: ThinkingLevel, context: Vec<ContextItem>) -> ModelRequest {
    let mut ask = request(Family::Chat, model, context);
    ask.params.thinking = level;
    ask
}

fn source(model: &str) -> ReplaySource {
    ReplaySource {
        family: Family::Chat,
        model: model.into(),
    }
}

fn member_str<'a>(body: &'a sonic_rs::Value, name: &str) -> Option<&'a str> {
    body.get(name).and_then(|value| value.as_str())
}

fn messages_of<'a>(body: &'a sonic_rs::Value, role: &str) -> Vec<&'a sonic_rs::Value> {
    body.get("messages")
        .and_then(|messages| messages.as_array())
        .expect("messages is an array")
        .iter()
        .filter(|message| member_str(message, "role") == Some(role))
        .collect()
}

fn read_file_tool(parameters: &str) -> Result<Arc<[ModelToolSpec]>, Box<dyn Error>> {
    Ok(Arc::from([ModelToolSpec {
        name: "read_file".into(),
        description: "Reads one file.".into(),
        parameters: RawJson::parse(parameters)?,
        grammar: None,
        defer_loading: false,
    }]))
}

const PLAIN_SCHEMA: &str =
    r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#;

fn tool_turn(source: ReplaySource, parts: Vec<AssistantPart>, call: &str) -> [ContextItem; 2] {
    [
        ContextItem::Assistant {
            source,
            replay: None,
            parts,
        },
        ContextItem::ToolResult {
            call: CallId::new(call),
            name: "read_file".into(),
            is_error: false,
            parts: vec![Part::Text {
                text: "fn main() {}".into(),
            }],
        },
    ]
}

fn call_part(call: &str) -> AssistantPart {
    AssistantPart::ToolCall {
        call: CallId::new(call),
        name: "read_file".into(),
        args: RawJson::parse(r#"{"path":"src/main.rs"}"#).expect("valid arguments"),
        replay: None,
    }
}

/// A history of three assistant tool turns: the model's own with its
/// reasoning, its own without any, and another model's with reasoning that
/// this host must never receive.
fn replay_history(model: &str) -> Vec<ContextItem> {
    let mut context = vec![user_text("Open main.")];
    context.extend(tool_turn(
        source(model),
        vec![
            AssistantPart::Thinking {
                text: REASONING.into(),
                replay: None,
            },
            call_part(CALL),
        ],
        CALL,
    ));
    context.extend(tool_turn(
        source(model),
        vec![call_part(CALL_WITHOUT_REASONING)],
        CALL_WITHOUT_REASONING,
    ));
    context.extend(tool_turn(
        source("another-model"),
        vec![
            AssistantPart::Thinking {
                text: "Foreign.".into(),
                replay: None,
            },
            call_part(CALL_OF_ANOTHER_MODEL),
        ],
        CALL_OF_ANOTHER_MODEL,
    ));
    context.push(user_text("Once more."));
    context
}

fn replay_turn(model: &str, level: ThinkingLevel) -> Result<ModelRequest, Box<dyn Error>> {
    let mut ask = turn(model, level, replay_history(model));
    ask.tools = read_file_tool(PLAIN_SCHEMA)?;
    Ok(ask)
}

/// The `reasoning_content` member of each assistant message that carries
/// tool calls, in order.
fn replayed_on_calls(body: &sonic_rs::Value) -> Vec<Option<String>> {
    messages_of(body, "assistant")
        .into_iter()
        .filter(|message| message.get("tool_calls").is_some())
        .map(|message| member_str(message, "reasoning_content").map(str::to_owned))
        .collect()
}

/// The `thinking.type` member of a body.
fn thinking_type(body: &sonic_rs::Value) -> Option<&str> {
    body.get("thinking")
        .and_then(|thinking| member_str(thinking, "type"))
}

/// The tool turns of [`replay_history`] on a host that needs the member on
/// every tool turn: its own reasoning, then an empty member for a turn with
/// none and for a turn of another model.
fn assert_tool_turns_carry_the_member(body: &sonic_rs::Value) {
    assert_eq!(
        replayed_on_calls(body),
        [
            Some(REASONING.to_owned()),
            Some(String::new()),
            Some(String::new())
        ],
        "{body:?}"
    );
}

#[tokio::test]
async fn a_requested_output_cap_is_sent_under_the_field_each_host_names() -> TestResult {
    for (host, model, field, other) in [
        (
            GROQ,
            "llama-3.3-70b-versatile",
            "max_completion_tokens",
            "max_tokens",
        ),
        (
            CEREBRAS,
            "gpt-oss-120b",
            "max_completion_tokens",
            "max_tokens",
        ),
        (MIMO, "mimo-v2.6-pro", "max_completion_tokens", "max_tokens"),
        (MOONSHOT, "kimi-k3", "max_tokens", "max_completion_tokens"),
        (
            DEEPSEEK,
            "deepseek-chat",
            "max_tokens",
            "max_completion_tokens",
        ),
    ] {
        let mut capped = turn(model, ThinkingLevel::Off, vec![user_text("Count.")]);
        capped.params.max_output_tokens = Some(2_048);
        let uncapped = turn(model, ThinkingLevel::Off, vec![user_text("Count.")]);
        let bodies = chat_bodies(&host, model, vec![capped, uncapped]).await?;
        let body = &bodies[0];
        assert_eq!(
            body.get(field).and_then(sonic_rs::JsonValueTrait::as_u64),
            Some(2_048),
            "{}: {body:?}",
            host.provider
        );
        assert!(
            body.get(other).is_none(),
            "{}: {other} is in {body:?}",
            host.provider
        );
        // A request that asks for no cap keeps the body it always had.
        for member in ["max_completion_tokens", "max_tokens"] {
            assert!(
                bodies[1].get(member).is_none(),
                "{}: {member} is in {:?}",
                host.provider,
                bodies[1]
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn cerebras_gpt_oss_takes_low_medium_high_and_cannot_turn_reasoning_off() -> TestResult {
    let at = |level| turn("gpt-oss-120b", level, vec![user_text("Weigh it.")]);
    let asked = [
        ThinkingLevel::Off,
        ThinkingLevel::Minimal,
        ThinkingLevel::Medium,
        ThinkingLevel::Xhigh,
    ];
    let bodies = chat_bodies(&CEREBRAS, "gpt-oss-120b", asked.map(at).to_vec()).await?;
    let sent: Vec<Option<&str>> = bodies
        .iter()
        .map(|body| member_str(body, "reasoning_effort"))
        .collect();
    // `off` is not offered by the host (no `none`), `minimal` rides up to
    // `low`, and `xhigh` rides down to `high`.
    assert_eq!(sent, [None, Some("low"), Some("medium"), Some("high")]);
    Ok(())
}

#[tokio::test]
async fn cerebras_sends_no_stream_options_and_the_other_hosts_keep_theirs() -> TestResult {
    // The Cerebras request schema has no `stream_options`, and the pinned
    // oh-my-pi row marks usage-in-streaming unsupported; Moonshot documents
    // the member.
    let ask = |model: &str| turn(model, ThinkingLevel::Off, vec![user_text("Count.")]);
    let cerebras = chat_bodies(&CEREBRAS, "gpt-oss-120b", vec![ask("gpt-oss-120b")]).await?;
    assert!(
        cerebras[0].get("stream_options").is_none(),
        "{:?}",
        cerebras[0]
    );
    let moonshot = chat_bodies(&MOONSHOT, "kimi-k3", vec![ask("kimi-k3")]).await?;
    assert_eq!(
        moonshot[0]
            .get("stream_options")
            .and_then(|options| options.get("include_usage"))
            .and_then(sonic_rs::JsonValueTrait::as_bool),
        Some(true),
        "{:?}",
        moonshot[0]
    );
    Ok(())
}

#[tokio::test]
async fn mimo_v2_6_toggles_thinking_with_an_object_and_replays_reasoning_on_tool_turns()
-> TestResult {
    // The current official Chat page lists `mimo-v2.6-*` and documents
    // `thinking.type` `enabled|disabled` (its own example sends `disabled`)
    // and no `reasoning_effort` member.
    let model = "mimo-v2.6-pro";
    let bodies = chat_bodies(
        &MIMO,
        model,
        vec![
            replay_turn(model, ThinkingLevel::High)?,
            replay_turn(model, ThinkingLevel::Off)?,
        ],
    )
    .await?;
    let (on, off) = (&bodies[0], &bodies[1]);
    assert_eq!(thinking_type(on), Some("enabled"), "{on:?}");
    assert_eq!(thinking_type(off), Some("disabled"), "{off:?}");
    for body in [on, off] {
        assert!(body.get("reasoning_effort").is_none(), "{body:?}");
        assert_tool_turns_carry_the_member(body);
    }
    Ok(())
}

#[tokio::test]
async fn mimo_v2_5_is_always_on_with_its_effort_and_replays_reasoning_on_tool_turns() -> TestResult
{
    // The pinned `mimo-v2.5` rows carry `supportsDisabledThinking:false`
    // (senpi forces it on `mimo-v2.5-pro`): thinking is on with its effort,
    // and `off` sends neither member, never `disabled` and never `none`.
    let model = "mimo-v2.5";
    let bodies = chat_bodies(
        &MIMO,
        model,
        vec![
            replay_turn(model, ThinkingLevel::High)?,
            replay_turn(model, ThinkingLevel::Off)?,
        ],
    )
    .await?;
    let (on, off) = (&bodies[0], &bodies[1]);
    assert_eq!(thinking_type(on), Some("enabled"), "{on:?}");
    assert_eq!(member_str(on, "reasoning_effort"), Some("high"), "{on:?}");
    assert!(off.get("thinking").is_none(), "{off:?}");
    assert!(off.get("reasoning_effort").is_none(), "{off:?}");
    for body in [on, off] {
        assert_tool_turns_carry_the_member(body);
    }
    Ok(())
}

#[tokio::test]
async fn an_unlisted_mimo_id_keeps_the_standard_body() -> TestResult {
    // No source describes `mimo-v3-*`: no switch, no replay, no effort
    // ceiling is invented for it. (This guard passes before the fix.)
    let model = "mimo-v3-flash";
    let bodies = chat_bodies(&MIMO, model, vec![replay_turn(model, ThinkingLevel::High)?]).await?;
    let body = &bodies[0];
    assert_eq!(
        member_str(body, "reasoning_effort"),
        Some("high"),
        "{body:?}"
    );
    assert!(body.get("thinking").is_none(), "{body:?}");
    assert_eq!(replayed_on_calls(body), [None, None, None], "{body:?}");
    Ok(())
}

#[tokio::test]
async fn kimi_k3_takes_its_effort_and_replays_reasoning_on_tool_turns() -> TestResult {
    let model = "kimi-k3";
    let levels = [
        ThinkingLevel::High,
        ThinkingLevel::Max,
        ThinkingLevel::Medium,
        ThinkingLevel::Off,
    ];
    let turns = levels
        .into_iter()
        .map(|level| replay_turn(model, level))
        .collect::<Result<Vec<_>, _>>()?;
    let bodies = chat_bodies(&MOONSHOT, model, turns).await?;
    // K3 names `low`, `high`, and `max`; `medium` rides down to `low`, and
    // `off` is not offered: no effort and no `thinking` object either way.
    let sent: Vec<Option<&str>> = bodies
        .iter()
        .map(|body| member_str(body, "reasoning_effort"))
        .collect();
    assert_eq!(sent, [Some("high"), Some("max"), Some("low"), None]);
    for body in &bodies {
        assert!(body.get("thinking").is_none(), "{body:?}");
        assert_tool_turns_carry_the_member(body);
    }
    Ok(())
}

#[tokio::test]
async fn kimi_k2_switches_thinking_with_an_object_and_replays_only_the_reasoning_it_has()
-> TestResult {
    // K2.6 can turn thinking off.
    let model = "kimi-k2.6";
    let bodies = chat_bodies(
        &MOONSHOT,
        model,
        vec![
            replay_turn(model, ThinkingLevel::High)?,
            replay_turn(model, ThinkingLevel::Off)?,
        ],
    )
    .await?;
    assert_eq!(thinking_type(&bodies[0]), Some("enabled"));
    assert_eq!(thinking_type(&bodies[1]), Some("disabled"));
    for body in &bodies {
        // The switch is the whole control: no effort. The reasoning of its
        // own tool turn replays, and a turn without reasoning sends no
        // member.
        assert!(body.get("reasoning_effort").is_none(), "{body:?}");
        assert_eq!(
            replayed_on_calls(body),
            [Some(REASONING.to_owned()), None, None],
            "{body:?}"
        );
    }

    // K2.7 Code cannot turn thinking off: `off` sends no member at all.
    let model = "kimi-k2.7-code";
    let bodies = chat_bodies(
        &MOONSHOT,
        model,
        vec![
            replay_turn(model, ThinkingLevel::High)?,
            replay_turn(model, ThinkingLevel::Off)?,
        ],
    )
    .await?;
    assert_eq!(thinking_type(&bodies[0]), Some("enabled"));
    assert!(bodies[1].get("thinking").is_none(), "{:?}", bodies[1]);
    for body in &bodies {
        assert!(body.get("reasoning_effort").is_none(), "{body:?}");
    }
    Ok(())
}

#[tokio::test]
async fn moonshot_tool_schemas_arrive_without_the_keywords_the_host_rejects() -> TestResult {
    // `format`, `title`, `examples`, `deprecated`, and `$schema` are
    // annotations at schema nodes; a property merely named `format` or
    // `title`, and a `default` that carries those names as data, are data
    // and stay. Numbers keep their exact text through the rewrite: `limit`
    // carries an integer past `u64`, a big enum value, and spellings a float
    // round trip would change.
    const SCHEMA: &str = r#"{"$schema":"http://json-schema.org/draft-07/schema#","title":"Read","type":"object","properties":{"limit":{"type":"integer","format":"int64","minimum":18446744073709551617,"maximum":1e3,"multipleOf":1.50,"enum":[12345678901234567890123,1.0],"default":0.10},"path":{"type":"string","title":"Path","format":"uri-reference","examples":["src/main.rs"]},"format":{"type":"string","enum":["raw","json"],"description":"Output format."},"title":{"type":"string"},"options":{"type":"object","default":{"format":"raw","title":"t"}},"at":{"anyOf":[{"type":"string","format":"date-time"},{"type":"null"}]},"lines":{"type":"array","items":{"type":"integer","deprecated":true}}},"required":["path"],"additionalProperties":false}"#;

    let model = "kimi-k3";
    let mut ask = turn(model, ThinkingLevel::Off, vec![user_text("Open main.")]);
    ask.tools = read_file_tool(SCHEMA)?;
    let bodies = chat_bodies(&MOONSHOT, model, vec![ask]).await?;
    let parameters = bodies[0]
        .get("tools")
        .and_then(|tools| tools.get(0usize))
        .and_then(|tool| tool.get("function"))
        .and_then(|function| function.get("parameters"))
        .expect("the tool is advertised");
    // Every annotation at a schema node is gone, at the root, under a
    // property, under a combiner branch, and under `items`.
    let wire = sonic_rs::to_string(parameters)?;
    for gone in [
        "$schema",
        "\"Read\"",
        "\"Path\"",
        "uri-reference",
        "examples",
        "date-time",
        "deprecated",
    ] {
        assert!(!wire.contains(gone), "{gone} is in {wire}");
    }
    let schema_at = |path: &[&str]| {
        let mut node = parameters;
        for step in path {
            node = node.get(*step)?;
        }
        sonic_rs::to_string(node).ok()
    };
    assert_eq!(
        schema_at(&["properties", "path"]).as_deref(),
        Some(r#"{"type":"string"}"#)
    );
    assert_eq!(
        schema_at(&["properties", "lines", "items"]).as_deref(),
        Some(r#"{"type":"integer"}"#)
    );
    // The properties named `format` and `title` keep their whole schemas.
    let format = schema_at(&["properties", "format"]).expect("the `format` property stays");
    assert!(format.contains(r#""enum":["raw","json"]"#), "{format}");
    assert!(
        format.contains(r#""description":"Output format.""#),
        "{format}"
    );
    assert_eq!(
        schema_at(&["properties", "title"]).as_deref(),
        Some(r#"{"type":"string"}"#)
    );
    // A `default` is data, not a schema: its members are never annotations.
    let default =
        schema_at(&["properties", "options", "default"]).expect("the default of `options` stays");
    assert!(default.contains(r#""format":"raw""#), "{default}");
    assert!(default.contains(r#""title":"t""#), "{default}");
    // The exact numeric text survives the rewrite.
    let limit = schema_at(&["properties", "limit"]).expect("the `limit` property stays");
    for digits in [
        "18446744073709551617",
        "12345678901234567890123",
        "1e3",
        "1.50",
        "1.0",
        "0.10",
    ] {
        assert!(limit.contains(digits), "{digits} is not in {limit}");
    }
    assert!(!limit.contains("format"), "{limit}");
    // What the host accepts is untouched.
    assert_eq!(schema_at(&["required"]).as_deref(), Some(r#"["path"]"#));
    assert_eq!(
        schema_at(&["additionalProperties"]).as_deref(),
        Some("false")
    );
    Ok(())
}
