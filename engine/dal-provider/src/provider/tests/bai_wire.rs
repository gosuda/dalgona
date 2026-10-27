//! Wire proofs of the built-in `bai` (B.AI) row, driven the way a host drives
//! it: a strict `[providers.bai]` table with an explicit loopback `base_url`,
//! a fake `BAI_API_KEY` taken from an [`EnvSnapshot`] only, the real
//! [`ProviderSet`], and the real request lifecycle against an owned loopback
//! fixture that captures the actual HTTP bytes.
//!
//! The expected values here come from the pinned upstream B.AI provider
//! (senpi pin `b8c1d3a8`, archive sha256 `13722b0a…57ed3a`), not from this
//! crate's row or wire definitions. Archive members:
//!
//! - `packages/ai/src/providers/data/bai.json` (sha256 `7e6861aa…32b99f`):
//!   `gpt-5.6-sol` rides `openai-responses`, `gemini-3.8-flash` rides
//!   `openai-completions`, `claude-sonnet-5` rides `anthropic-messages`.
//! - `packages/ai/test/bai-tool-schema.test.ts` (`WIRE_APIS`): one selector
//!   per wire and the URL it reaches: `gpt-5.6-sol` at `/v1/responses`,
//!   `gemini-3.8-flash` at `/v1/chat/completions`, `claude-sonnet-5` at
//!   `/v1/messages`; a union-root tool schema is delivered as one object
//!   schema (`type` `object`, properties `name`, `op`, `pool_id`, `required`
//!   `["op"]`); an object-root schema is untouched; the output-token member is
//!   `max_output_tokens`, `max_completion_tokens`, `max_tokens` respectively,
//!   and no other.
//! - `packages/ai/src/providers/bai.ts` lines 101 to 104 and
//!   `packages/ai/test/bai-provider.test.ts` lines 150 to 172: a custom gateway
//!   base such as `https://staging.b.ai/v1/` serves Chat and Responses at that
//!   base and Anthropic at the same base with its `/v1` removed, so every wire
//!   keeps its `/v1/...` path under one operator host. The explicit base wins
//!   over the Anthropic bare-host default, so all traffic stays on loopback.
//! - `packages/ai/src/providers/bai.ts` line 113: the key variable is
//!   `BAI_API_KEY`. Chat and Responses carry it as `Authorization: Bearer`
//!   (`packages/ai/src/api/openai-responses.ts` lines 1073 to 1074) and Chat
//!   builds its `OpenAI` client with `apiKey` (`openai-completions.ts` lines 1148
//!   to 1150), which the SDK sends the same way.
//!
//! No case here reads a row field, a census, a source marker, a default
//! literal, or an empty capture as proof; every assertion is on bytes the
//! client put on the wire or on the events it decoded from a real SSE dialect
//! fixture.
//!
//! Network safety: every base is an explicit loopback URL and every request is
//! captured by an owned fixture. The Anthropic cases (whose row default would
//! name the public operator host if the explicit base lost) carry
//! `anthropic` in their names so they can be run or skipped by name; run them
//! only where the explicit base is already proven to win or public egress is
//! denied. The refusal case opens no request at all.

use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use super::*;

/// The fake key the environment snapshot carries; it never leaves loopback.
const BAI_KEY: &str = "bai-wire-test-key";
const SYSTEM_TEXT: &str = "BAI wire probe system prompt.";
const USER_TEXT: &str = "BAI wire probe user turn.";
/// The requested output cap of the pinned `max_tokens: 16` probe.
const OUTPUT_CAP: u32 = 16;
/// The bound on every hop that waits on the fixture or the stream.
const BOUND: Duration = Duration::from_secs(10);

/// `bai-tool-schema.test.ts`: `Type.Union` of two objects, as `TypeBox` emits it.
const UNION_ROOT_SCHEMA: &str = r#"{"anyOf":[{"type":"object","properties":{"op":{"type":"string","const":"create"},"name":{"type":"string"}},"required":["op","name"]},{"type":"object","properties":{"op":{"type":"string","const":"inspect"},"pool_id":{"type":"string"}},"required":["op","pool_id"]}]}"#;
/// `bai-tool-schema.test.ts`: the object-root control.
const OBJECT_ROOT_SCHEMA: &str =
    r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#;

/// Ids absent from `bai.json`, including ones shaped like a pinned family: the
/// pinned catalog is exact ids, never a prefix.
const UNPINNED: [&str; 4] = [
    "claude-unlisted-9",
    "gpt-unlisted-9",
    "gemini-unlisted-9",
    "unlisted-model",
];

/// A `[providers.bai]` table that names nothing but the loopback base, over an
/// environment that holds only the fake key.
fn bai_set(dir: &TestDir, base: &str) -> Result<ProviderSet, Box<dyn Error>> {
    preset_set(
        dir,
        &format!("[providers.bai]\nbase_url = '{base}'\n"),
        &[("BAI_API_KEY", BAI_KEY)],
    )
}

/// The route a catalog resolution of `model` on `bai` hands the set.
fn bai_route(family: Family, model: &str) -> ResolvedModel {
    let mut route = resolved("bai", family, model);
    if family == Family::Anthropic {
        route.entry.thinking = ThinkingSupport::UnknownAdaptive;
    }
    route
}

fn tool_spec(schema: &str) -> Result<ModelToolSpec, Box<dyn Error>> {
    Ok(ModelToolSpec {
        name: "probe_tool".into(),
        description: "Schema compatibility probe.".into(),
        parameters: RawJson::parse(schema)?,
        grammar: None,
        defer_loading: false,
    })
}

/// One turn on `model` through the real lifecycle.
struct Probe {
    model: &'static str,
    family: Family,
    reply: String,
    tools: Vec<ModelToolSpec>,
    cap: Option<u32>,
    /// Text appended to the fixture's `/v1` base, to exercise a base spelled
    /// with a trailing slash.
    base_tail: &'static str,
}

/// What the fixture captured and what the client decoded.
struct Exchange {
    requests: usize,
    /// The request head, lowercased, without the body.
    head: String,
    body_text: String,
    body: sonic_rs::Value,
    text: String,
    /// `(input, cached input, output, cache write)` of the one usage event.
    usage: Option<(u64, u64, u64, u64)>,
    usage_events: usize,
    stops: usize,
    ends_with_stop: bool,
}

fn head_text(captured: &[u8]) -> String {
    let end = captured
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or(captured.len());
    String::from_utf8_lossy(&captured[..end]).to_ascii_lowercase()
}

async fn exchange(probe: Probe) -> Result<Exchange, Box<dyn Error>> {
    let dir = TestDir::new()?;
    let (base, mut captured) = server("/v1", vec![stream_reply(probe.reply)]).await?;
    let set = bai_set(&dir, &format!("{base}{}", probe.base_tail))?;
    let provider = set.provider(bai_route(probe.family, probe.model))?;
    let mut ask = request(probe.family, probe.model, vec![user_text(USER_TEXT)]);
    ask.system = Arc::from(SYSTEM_TEXT);
    ask.tools = Arc::<[ModelToolSpec]>::from(probe.tools);
    ask.params.max_output_tokens = probe.cap;
    let cancel = CancellationToken::new();
    let stream = tokio::time::timeout(
        BOUND,
        provider.open(SessionId::new_v7(), &ask, &[], notice_sink(), &cancel),
    )
    .await
    .map_err(|_| io::Error::other("the BAI turn did not open within the bound"))??;
    let events = tokio::time::timeout(BOUND, collect(stream))
        .await
        .map_err(|_| io::Error::other("the BAI stream did not finish within the bound"))??;
    let joined = tokio::time::timeout(BOUND, captured.join_next())
        .await
        .map_err(|_| io::Error::other("the loopback fixture was never contacted"))?;
    let finished = joined.ok_or_else(|| io::Error::other("the loopback fixture left no result"))?;
    let requests = finished??;
    let first = requests
        .first()
        .ok_or_else(|| io::Error::other("the loopback fixture captured no request"))?;
    let body_bytes = request_body(first);
    Ok(Exchange {
        requests: requests.len(),
        head: head_text(first),
        body_text: String::from_utf8_lossy(body_bytes).into_owned(),
        body: sonic_rs::from_slice::<sonic_rs::Value>(body_bytes)?,
        text: events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect(),
        usage: events.iter().find_map(|event| match event {
            StreamEvent::Usage { usage } => Some((
                usage.input_tokens,
                usage.cached_input_tokens,
                usage.output_tokens,
                usage.cache_write_tokens,
            )),
            _ => None,
        }),
        usage_events: events
            .iter()
            .filter(|event| matches!(event, StreamEvent::Usage { .. }))
            .count(),
        stops: events
            .iter()
            .filter(|event| matches!(event, StreamEvent::Stop { .. }))
            .count(),
        ends_with_stop: matches!(events.last(), Some(StreamEvent::Stop { .. })),
    })
}

/// The credential rides the wire the way the wire's API carries it, once, and
/// nowhere else.
///
/// Chat and Responses: `Authorization: Bearer` and no `x-api-key` (pin
/// `openai-responses.ts:1073-1074`, Chat `openai-completions.ts:1148-1150`,
/// `OpenAI` SDK `apiKey`). `Anthropic`: the pin
/// builds its client with `apiKey` set and `authToken: null`
/// (`anthropic-messages.ts:2205-2206`), which implies `x-api-key` (the SDK
/// default; the SDK is not in the archive). The private v2 offline proof
/// (`wire_offline_anthropic.rs:200-203`) expects `authorization: bearer` and
/// no `x-api-key`. Neither source settles what the operator's server requires,
/// so this case requires exactly one of the two carriers to hold the key,
/// never both and never another style; the row's owner pins one and tightens
/// this arm.
fn assert_credential(head: &str, family: Family, label: &str) {
    let bearer = format!("authorization: bearer {BAI_KEY}");
    let api_key = format!("x-api-key: {BAI_KEY}");
    if family == Family::Anthropic {
        assert!(
            head.contains("anthropic-version: 2023-06-01"),
            "{label}: {head}"
        );
        let carriers = usize::from(head.contains(&bearer)) + usize::from(head.contains(&api_key));
        assert_eq!(carriers, 1, "{label}: {head}");
        assert!(!head.contains("cf-aig-authorization:"), "{label}: {head}");
        assert!(!head.contains("\napi-key:"), "{label}: {head}");
    } else {
        assert!(head.contains(&bearer), "{label}: {head}");
        assert!(!head.contains("x-api-key:"), "{label}: {head}");
    }
    let request_line = head.lines().next().unwrap_or_default();
    assert!(!request_line.contains(BAI_KEY), "{label}: {request_line}");
}

/// Whether a `user` turn under `body[key]` carries `text`.
fn user_turn_carries(body: &sonic_rs::Value, key: &str, text: &str) -> bool {
    body.get(key)
        .and_then(|turns| turns.as_array())
        .is_some_and(|turns| {
            turns.iter().any(|turn| {
                turn.get("role").and_then(|role| role.as_str()) == Some("user")
                    && sonic_rs::to_string(turn).is_ok_and(|wire| wire.contains(text))
            })
        })
}

fn string_member<'a>(body: &'a sonic_rs::Value, name: &str) -> Option<&'a str> {
    body.get(name).and_then(|member| member.as_str())
}

fn number_member(body: &sonic_rs::Value, name: &str) -> Option<u64> {
    body.get(name).and_then(sonic_rs::JsonValueTrait::as_u64)
}

/// The parameters schema of the one advertised tool, wherever the wire keeps
/// it: `parameters` (Responses), `function.parameters` (Chat), or
/// `input_schema` (Anthropic).
fn only_tool_schema(body: &sonic_rs::Value) -> Option<&sonic_rs::Value> {
    let tool = body.get("tools")?.get(0usize)?;
    tool.get("parameters")
        .or_else(|| {
            tool.get("function")
                .and_then(|function| function.get("parameters"))
        })
        .or_else(|| tool.get("input_schema"))
}

fn property_names(schema: &sonic_rs::Value) -> Vec<String> {
    let mut names: Vec<String> = schema
        .get("properties")
        .and_then(|properties| properties.as_object())
        .map(|properties| properties.iter().map(|(name, _)| name.to_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

fn required_names(schema: &sonic_rs::Value) -> Option<Vec<String>> {
    schema.get("required")?.as_array().map(|items| {
        items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect()
    })
}

/// A Chat dialect stream: two text chunks, then the stop chunk with usage
/// (`prompt_tokens` 11, `completion_tokens` 7).
fn gemini_chat_reply() -> String {
    concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Bonjour \"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"from Gemini.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7}}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned()
}

/// A Responses dialect stream: two text deltas, then completion with usage
/// (`input_tokens` 13, `output_tokens` 5).
fn sol_responses_reply() -> String {
    concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello \"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"from Sol.\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"output\":[],\"usage\":{\"input_tokens\":13,\"output_tokens\":5}}}\n\n",
    )
    .to_owned()
}

/// A Messages dialect stream: two text deltas, input usage on `message_start`
/// (17) and the cumulative output usage on `message_delta` (9).
fn sonnet_messages_reply() -> String {
    concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-bai\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-5\",\"stop_reason\":null,\"usage\":{\"input_tokens\":17,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello \"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"from Sonnet.\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":9}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    )
    .to_owned()
}

/// One pinned selector and everything its wire must show.
struct WireCase {
    model: &'static str,
    family: Family,
    /// The pinned URL path (`bai-tool-schema.test.ts` `WIRE_APIS`) on the
    /// loopback origin.
    path: &'static str,
    reply: fn() -> String,
    /// The whole decoded assistant text of `reply`.
    text: &'static str,
    input_tokens: u64,
    output_tokens: u64,
    /// The one output-token member the wire documents.
    cap_member: &'static str,
    foreign_cap_members: &'static [&'static str],
    /// The body member that holds the conversation turns.
    turns: &'static str,
}

const GEMINI: WireCase = WireCase {
    model: "gemini-3.8-flash",
    family: Family::Chat,
    path: "/v1/chat/completions",
    reply: gemini_chat_reply,
    text: "Bonjour from Gemini.",
    input_tokens: 11,
    output_tokens: 7,
    cap_member: "max_completion_tokens",
    foreign_cap_members: &["max_tokens", "max_output_tokens"],
    turns: "messages",
};

const SOL: WireCase = WireCase {
    model: "gpt-5.6-sol",
    family: Family::Responses,
    path: "/v1/responses",
    reply: sol_responses_reply,
    text: "Hello from Sol.",
    input_tokens: 13,
    output_tokens: 5,
    cap_member: "max_output_tokens",
    foreign_cap_members: &["max_tokens", "max_completion_tokens"],
    turns: "input",
};

const SONNET: WireCase = WireCase {
    model: "claude-sonnet-5",
    family: Family::Anthropic,
    path: "/v1/messages",
    reply: sonnet_messages_reply,
    text: "Hello from Sonnet.",
    input_tokens: 17,
    output_tokens: 9,
    cap_member: "max_tokens",
    foreign_cap_members: &["max_completion_tokens", "max_output_tokens"],
    turns: "messages",
};

/// One real turn on a pinned selector, through an explicit loopback base,
/// with the request and the decoded response both checked in full.
async fn assert_wire(case: &WireCase) -> TestResult {
    let label = case.model;
    let seen = exchange(Probe {
        model: case.model,
        family: case.family,
        reply: (case.reply)(),
        tools: Vec::new(),
        cap: Some(OUTPUT_CAP),
        base_tail: "",
    })
    .await?;

    assert_eq!(seen.requests, 1, "{label} sent one request");
    assert!(
        seen.head.starts_with(&format!("post {} ", case.path)),
        "{label}: {}",
        seen.head
    );
    assert_credential(&seen.head, case.family, label);
    assert!(
        !seen.body_text.contains(BAI_KEY),
        "{label}: key in the body"
    );

    assert_eq!(
        string_member(&seen.body, "model"),
        Some(case.model),
        "{label}"
    );
    assert_eq!(
        seen.body
            .get("stream")
            .and_then(sonic_rs::JsonValueTrait::as_bool),
        Some(true),
        "{label}: {}",
        seen.body_text
    );
    assert!(
        user_turn_carries(&seen.body, case.turns, USER_TEXT),
        "{label}: the user turn is not under `{}`: {}",
        case.turns,
        seen.body_text
    );
    assert!(
        seen.body_text.contains(SYSTEM_TEXT),
        "{label}: the system prompt is missing: {}",
        seen.body_text
    );
    assert_eq!(
        number_member(&seen.body, case.cap_member),
        Some(u64::from(OUTPUT_CAP)),
        "{label}: {}",
        seen.body_text
    );
    for member in case.foreign_cap_members {
        assert!(
            seen.body.get(*member).is_none(),
            "{label}: {member} is in {}",
            seen.body_text
        );
    }

    assert_eq!(seen.text, case.text, "{label}");
    assert_eq!(
        seen.usage,
        Some((case.input_tokens, 0, case.output_tokens, 0)),
        "{label}"
    );
    assert_ne!(case.input_tokens, case.output_tokens);
    assert_eq!(seen.usage_events, 1, "{label}");
    assert_eq!(seen.stops, 1, "{label}");
    assert!(seen.ends_with_stop, "{label}");

    // The pinned custom gateway spelling `https://staging.b.ai/v1/` ends in a
    // slash; the same wire path must come out.
    let slashed = exchange(Probe {
        model: case.model,
        family: case.family,
        reply: (case.reply)(),
        tools: Vec::new(),
        cap: None,
        base_tail: "/",
    })
    .await?;
    assert!(
        slashed.head.starts_with(&format!("post {} ", case.path)),
        "{label} on a trailing-slash base: {}",
        slashed.head
    );
    assert_eq!(slashed.text, case.text, "{label} on a trailing-slash base");
    Ok(())
}

/// The union-root tool schema reaches the wire as one object schema and the
/// object-root control is untouched (`bai-tool-schema.test.ts`).
async fn assert_schemas(case: &WireCase) -> TestResult {
    let label = case.model;
    let plain = (case.reply)();

    let union = exchange(Probe {
        model: case.model,
        family: case.family,
        reply: plain.clone(),
        tools: vec![tool_spec(UNION_ROOT_SCHEMA)?],
        cap: None,
        base_tail: "",
    })
    .await?;
    assert!(
        union.head.starts_with(&format!("post {} ", case.path)),
        "{label}: {}",
        union.head
    );
    let schema = only_tool_schema(&union.body)
        .ok_or_else(|| io::Error::other(format!("{label} sent no tool: {}", union.body_text)))?;
    assert_eq!(
        string_member(schema, "type"),
        Some("object"),
        "{label}: {}",
        union.body_text
    );
    assert_eq!(
        property_names(schema),
        ["name", "op", "pool_id"],
        "{label}: {}",
        union.body_text
    );
    assert_eq!(
        required_names(schema),
        Some(vec!["op".to_owned()]),
        "{label}: {}",
        union.body_text
    );

    let object = exchange(Probe {
        model: case.model,
        family: case.family,
        reply: plain,
        tools: vec![tool_spec(OBJECT_ROOT_SCHEMA)?],
        cap: None,
        base_tail: "",
    })
    .await?;
    let schema = only_tool_schema(&object.body)
        .ok_or_else(|| io::Error::other(format!("{label} sent no tool: {}", object.body_text)))?;
    assert_eq!(
        string_member(schema, "type"),
        Some("object"),
        "{label}: {}",
        object.body_text
    );
    assert_eq!(property_names(schema), ["path"], "{label}");
    Ok(())
}

/// Pinned `gemini-3.8-flash` rides the Chat wire.
#[tokio::test]
async fn bai_gemini_selector_reaches_the_pinned_chat_wire() -> TestResult {
    assert_wire(&GEMINI).await
}

/// Pinned `gpt-5.6-sol` rides the Responses wire.
#[tokio::test]
async fn bai_gpt_selector_reaches_the_pinned_responses_wire() -> TestResult {
    assert_wire(&SOL).await
}

/// Pinned `claude-sonnet-5` rides the Messages wire at the explicit base with
/// its `/v1` removed and re-added by the wire, never at the operator host.
#[tokio::test]
async fn bai_claude_selector_reaches_the_pinned_anthropic_wire_on_the_explicit_base() -> TestResult
{
    assert_wire(&SONNET).await
}

#[tokio::test]
async fn bai_union_root_tool_schema_is_one_object_on_the_chat_and_responses_wires() -> TestResult {
    assert_schemas(&GEMINI).await?;
    assert_schemas(&SOL).await
}

#[tokio::test]
async fn bai_union_root_tool_schema_is_one_object_on_the_anthropic_wire() -> TestResult {
    assert_schemas(&SONNET).await
}

/// The row binds the three pinned selectors from a strict base-only
/// configuration (no `api`, no key variable of its own), and ids outside
/// `bai.json`, including ones shaped like a pinned family, bind on no wire.
/// Nothing is opened: the fixture is a closed loopback port and the refusal
/// must come from binding the route, not from a failed connection or a
/// missing credential. The positive control keeps a row that refuses every
/// model from passing.
#[tokio::test]
async fn ids_outside_the_pinned_selectors_are_refused_before_any_request() -> TestResult {
    let dir = TestDir::new()?;
    let (base, _closed) = server("/v1", Vec::new()).await?;
    let set = bai_set(&dir, &base)?;
    for case in [&GEMINI, &SOL, &SONNET] {
        if let Err(error) = set.provider(bai_route(case.family, case.model)) {
            return Err(io::Error::other(format!(
                "bai did not bind the pinned {} as {:?}: {error:?}",
                case.model, case.family
            ))
            .into());
        }
    }
    for id in UNPINNED {
        for family in [Family::Chat, Family::Responses, Family::Anthropic] {
            match set.provider(bai_route(family, id)) {
                Err(ProviderError::NoCredentials { .. }) => {
                    return Err(io::Error::other(format!(
                        "{id} as {family:?} was refused for want of a credential, not for the missing selector"
                    ))
                    .into());
                }
                Err(_) => {}
                Ok(_) => {
                    return Err(io::Error::other(format!(
                        "bai bound {id} as {family:?} though no pinned selector names it"
                    ))
                    .into());
                }
            }
        }
    }
    Ok(())
}
