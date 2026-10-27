//! Wire proofs that `RequestParams::max_output_tokens` reaches the plain
//! `OpenAI` Responses and Anthropic bodies through the real request
//! lifecycle: a requested cap lowers what the host may generate, the catalog
//! row is the hard cap, an absent cap keeps each family's default, and an
//! Anthropic thinking budget is planned inside the cap.
//!
//! Every case builds the real [`ProviderSet`], resolves a catalog row with a
//! known output ceiling, and runs the lifecycle against a loopback server, so
//! the thinking plan and the body are the ones a host gets. Nothing reaches a
//! vendor.

use sonic_rs::JsonValueTrait;

use super::*;
use crate::{
    family::{
        azure::{self, AzureRequest, AzureSettings},
        copilot::{self, CopilotKey, CopilotRequest, Integration},
        droid::{self, DroidRequest, RoutePolicy},
    },
    thinking::{ThinkingPlan, WireThinking},
};

const RESPONSES_KEY: &str = "RESPONSES_CAP_KEY";
const ANTHROPIC_KEY: &str = "ANTHROPIC_CAP_KEY";

fn anthropic_text_reply() -> String {
    concat!(
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"id":"msg-1","type":"message","role":"assistant","content":[],"model":"claude-cap","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1}}}"#,
        "\n\nevent: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        "\n\nevent: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        "\n\nevent: content_block_stop\n",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "\n\nevent: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":1}}"#,
        "\n\nevent: message_stop\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    )
    .to_owned()
}

/// Runs `asks` as consecutive turns of one provider row against a loopback
/// server and returns the JSON body each turn sent.
async fn turn_bodies(
    family: Family,
    key_env: &str,
    row: ResolvedModel,
    reply: fn() -> String,
    asks: &[ModelRequest],
) -> Result<Vec<sonic_rs::Value>, Box<dyn Error>> {
    let dir = TestDir::new()?;
    let replies = asks.iter().map(|_| stream_reply(reply())).collect();
    let (base, mut captured) = server("/v1", replies).await?;
    let host = entry(
        &row.provider,
        family,
        base,
        Some(key_env),
        1,
        Transport::Https,
    );
    let set = ProviderSet::new(
        &config(vec![host], 0),
        identity(),
        EnvSnapshot::test(&[(key_env, "cap-secret")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(row)?;
    for ask in asks {
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
    assert_eq!(requests.len(), asks.len());
    let mut bodies = Vec::with_capacity(requests.len());
    for wire in &requests {
        bodies.push(sonic_rs::from_slice::<sonic_rs::Value>(request_body(wire))?);
    }
    Ok(bodies)
}

fn number(body: &sonic_rs::Value, name: &str) -> Option<u64> {
    body.get(name).and_then(sonic_rs::JsonValueTrait::as_u64)
}

/// The `thinking` member of a body: its type and its budget, when it has one.
fn thinking_of(body: &sonic_rs::Value) -> (Option<&str>, Option<u64>) {
    let thinking = body.get("thinking");
    (
        thinking
            .and_then(|member| member.get("type"))
            .and_then(|kind| kind.as_str()),
        thinking
            .and_then(|member| member.get("budget_tokens"))
            .and_then(sonic_rs::JsonValueTrait::as_u64),
    )
}

/// The output cap and thinking member one Anthropic-style body sent, read
/// back member by member so each `None`/`Some` boundary stays its own fact.
#[derive(Debug, PartialEq, Eq)]
struct SentCap<'a> {
    max_tokens: Option<u64>,
    thinking_kind: Option<&'a str>,
    thinking_budget: Option<u64>,
}

fn responses_ask(cap: Option<u32>) -> ModelRequest {
    let mut ask = request(Family::Responses, "gpt-cap", vec![user_text("Count.")]);
    ask.params.max_output_tokens = cap;
    ask
}

fn anthropic_ask(level: ThinkingLevel, cap: Option<u32>) -> ModelRequest {
    let mut ask = request(Family::Anthropic, "claude-cap", vec![user_text("Count.")]);
    ask.params.thinking = level;
    ask.params.max_output_tokens = cap;
    ask
}

/// An Anthropic row whose catalog ceiling is 64000 tokens and whose thinking
/// is the budget kind, so a cap and a thinking budget must share one ceiling.
fn anthropic_row() -> ResolvedModel {
    let mut row = resolved("anthropic-cap", Family::Anthropic, "claude-cap");
    row.entry.max_output = Some(64_000);
    row.entry.thinking = ThinkingSupport::Budget { can_disable: true };
    row
}

fn plain_plan() -> ThinkingPlan {
    ThinkingPlan {
        level: ThinkingLevel::Medium,
        wire: WireThinking::OpenAi {
            effort: Some("medium"),
        },
        temperature: None,
        notices: Vec::new(),
    }
}

fn body_number(body: &[u8], name: &str) -> Result<Option<u64>, Box<dyn Error>> {
    let body: sonic_rs::Value = sonic_rs::from_slice(body)?;
    Ok(number(&body, name))
}

#[test]
fn droid_sends_a_requested_cap_where_native_traffic_does_and_omits_it_on_gpt() -> TestResult {
    // Native Droid traffic (omp pin fixture `factory-droid-native-requests.json`)
    // carries no cap on `gpt-*` Responses requests and `max_output_tokens` on
    // `grok-*`; the pin's `factory-droid.kdl` also states
    // `omit-max-output-tokens` for `gpt-*`.
    let plan = plain_plan();
    let policy = RoutePolicy {
        max_output: Some(16_000),
        ..Default::default()
    };
    let sent = |model: &str, cap: Option<u32>| -> Result<Option<u64>, Box<dyn Error>> {
        let mut ask = request(Family::Droid, model, vec![user_text("Count.")]);
        ask.params.max_output_tokens = cap;
        let wire = droid::build_wire(&DroidRequest {
            request: &ask,
            thinking: &plan,
            base_url: "https://api.factory.ai",
            upstream: "openai",
            session: SessionId::new_v7(),
            request_id: "req-cap",
            bearer: "workos-token",
            policy: &policy,
        })?;
        body_number(&wire.body, "max_output_tokens")
    };
    assert_eq!(sent("gpt-5.5", Some(1_024))?, None);
    assert_eq!(sent("gpt-5.5", None)?, None);
    assert_eq!(sent("grok-4.7", Some(1_024))?, Some(1_024));
    assert_eq!(sent("grok-4.7", Some(70_000))?, Some(16_000));
    assert_eq!(sent("grok-4.7", None)?, None);
    Ok(())
}

#[test]
fn azure_responses_sends_a_requested_cap() -> TestResult {
    // Azure OpenAI documents `max_output_tokens` on the Responses API
    // (Microsoft Learn, "Azure OpenAI reasoning models"), and omp's Azure
    // adapter sends it through the shared Responses sampling mapping.
    let plan = plain_plan();
    let settings = AzureSettings::default();
    let credential = Credential::ApiKey {
        key: SecretString::from("az-secret"),
    };
    let sent = |cap: Option<u32>| -> Result<Option<u64>, Box<dyn Error>> {
        let mut ask = request(Family::Responses, "gpt-5.5", vec![user_text("Count.")]);
        ask.params.max_output_tokens = cap;
        let wire = azure::build(&AzureRequest {
            request: &ask,
            thinking: &plan,
            reasoning_summary: false,
            max_output: Some(4_096),
            provider: "azure",
            base_url: "https://res.openai.azure.com/openai/v1",
            settings: &settings,
            credential: &credential,
        })?;
        body_number(&wire.body, "max_output_tokens")
    };
    assert_eq!(sent(Some(1_024))?, Some(1_024));
    assert_eq!(sent(Some(70_000))?, Some(4_096));
    assert_eq!(sent(None)?, None);
    Ok(())
}

#[test]
fn copilot_responses_sends_a_requested_cap_held_to_the_catalog_ceiling() -> TestResult {
    // omp's GitHub Copilot rows have no `omit-max-output-tokens` policy and
    // send the cap through the shared Responses builder.
    let plan = plain_plan();
    let key = CopilotKey::parse("gho_cap");
    let integration = Integration::Chat;
    let sent = |cap: Option<u32>| -> Result<Option<u64>, Box<dyn Error>> {
        let mut ask = request(Family::Responses, "gpt-5.6-sol", vec![user_text("Count.")]);
        ask.params.max_output_tokens = cap;
        let wire = copilot::build(&CopilotRequest {
            request: &ask,
            thinking: &plan,
            max_output: Some(16_384),
            display_supported: false,
            reasoning_summary: false,
            base_url: copilot::PERSONAL_BASE_URL,
            key: &key,
            integration: &integration,
        })?;
        body_number(&wire.body, "max_output_tokens")
    };
    assert_eq!(sent(Some(1_024))?, Some(1_024));
    assert_eq!(sent(Some(70_000))?, Some(16_384));
    assert_eq!(sent(None)?, None);
    Ok(())
}

#[tokio::test]
async fn a_responses_turn_sends_the_requested_cap_held_to_the_catalog_ceiling() -> TestResult {
    // `resolved` gives the row a 4096-token output ceiling.
    let asks = [
        responses_ask(Some(1_024)),
        responses_ask(Some(4_096)),
        responses_ask(Some(70_000)),
        responses_ask(None),
    ];
    let bodies = turn_bodies(
        Family::Responses,
        RESPONSES_KEY,
        resolved("responses-cap", Family::Responses, "gpt-cap"),
        responses_reply,
        &asks,
    )
    .await?;
    let sent: Vec<Option<u64>> = bodies
        .iter()
        .map(|body| number(body, "max_output_tokens"))
        .collect();
    // A lower request is sent as asked, a larger one is held to the ceiling,
    // and no request keeps the host default instead of the catalog value.
    assert_eq!(sent, [Some(1_024), Some(4_096), Some(4_096), None]);
    Ok(())
}

#[tokio::test]
async fn an_anthropic_turn_sends_the_requested_cap_held_to_the_catalog_ceiling() -> TestResult {
    let asks = [
        anthropic_ask(ThinkingLevel::Off, Some(2_048)),
        anthropic_ask(ThinkingLevel::Off, Some(40_000)),
        anthropic_ask(ThinkingLevel::Off, Some(100_000)),
        anthropic_ask(ThinkingLevel::Off, None),
    ];
    let bodies = turn_bodies(
        Family::Anthropic,
        ANTHROPIC_KEY,
        anthropic_row(),
        anthropic_text_reply,
        &asks,
    )
    .await?;
    let sent: Vec<Option<u64>> = bodies
        .iter()
        .map(|body| number(body, "max_tokens"))
        .collect();
    // Lower than the default, above the default but inside the ceiling, past
    // the ceiling, and no request at all (the 32000 default).
    assert_eq!(
        sent,
        [Some(2_048), Some(40_000), Some(64_000), Some(32_000)]
    );
    Ok(())
}

#[tokio::test]
async fn a_droid_anthropic_budget_plan_stays_inside_the_requested_cap() -> TestResult {
    // A Droid request routes by its own family, not `Family::Anthropic`, yet
    // its `claude-*` models take the Anthropic wire: the thinking budget and
    // `max_tokens` they send must fit the requested cap as well.
    let dir = TestDir::new()?;
    let caps = [Some(4_096), Some(1_500), None];
    let replies = caps
        .iter()
        .map(|_| stream_reply(anthropic_text_reply()))
        .collect();
    let (base, mut captured) = server("", replies).await?;
    stored_oauth(&dir, "factory-droid", "workos-token")?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "factory-droid",
                Family::Droid,
                base,
                None,
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[]),
        dir.path(),
        dir.path(),
    )?;
    let mut row = resolved("factory-droid", Family::Droid, "claude-sonnet-5");
    row.entry.max_output = Some(64_000);
    row.entry.thinking = ThinkingSupport::Budget { can_disable: true };
    let provider = set.provider(row)?;
    for cap in caps {
        let mut ask = request(Family::Droid, "claude-sonnet-5", vec![user_text("Count.")]);
        ask.params.thinking = ThinkingLevel::High;
        ask.params.max_output_tokens = cap;
        let stream = provider
            .open(
                SessionId::new_v7(),
                &ask,
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?;
        collect(stream).await?;
    }
    let requests = captured.join_next().await.expect("server completes")??;
    assert_eq!(requests.len(), caps.len());
    let mut bodies = Vec::with_capacity(requests.len());
    for wire in &requests {
        bodies.push(sonic_rs::from_slice::<sonic_rs::Value>(request_body(wire))?);
    }
    let observed = bodies
        .iter()
        .map(|body| {
            let (thinking_kind, thinking_budget) = thinking_of(body);
            SentCap {
                max_tokens: number(body, "max_tokens"),
                thinking_kind,
                thinking_budget,
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        [
            // The cap is below the 16384 budget the level asks for: the
            // budget shrinks to leave 1024 tokens of visible output.
            SentCap {
                max_tokens: Some(4_096),
                thinking_kind: Some("enabled"),
                thinking_budget: Some(3_072),
            },
            // Under 2048 no budget of at least 1024 fits: thinking is off.
            SentCap {
                max_tokens: Some(1_500),
                thinking_kind: Some("disabled"),
                thinking_budget: None,
            },
            // No request keeps what a Droid Anthropic turn always sent: the
            // catalog ceiling and the full budget.
            SentCap {
                max_tokens: Some(64_000),
                thinking_kind: Some("enabled"),
                thinking_budget: Some(16_384),
            },
        ]
    );
    Ok(())
}

#[tokio::test]
async fn an_anthropic_thinking_budget_is_planned_inside_the_requested_cap() -> TestResult {
    let asks = [
        anthropic_ask(ThinkingLevel::High, Some(4_096)),
        anthropic_ask(ThinkingLevel::High, Some(40_000)),
        anthropic_ask(ThinkingLevel::High, Some(1_500)),
        anthropic_ask(ThinkingLevel::High, None),
    ];
    let bodies = turn_bodies(
        Family::Anthropic,
        ANTHROPIC_KEY,
        anthropic_row(),
        anthropic_text_reply,
        &asks,
    )
    .await?;
    let observed = bodies
        .iter()
        .map(|body| {
            let (thinking_kind, thinking_budget) = thinking_of(body);
            SentCap {
                max_tokens: number(body, "max_tokens"),
                thinking_kind,
                thinking_budget,
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        [
            // The cap is below the 16384 budget the level asks for: the
            // budget shrinks to leave 1024 tokens of visible output.
            SentCap {
                max_tokens: Some(4_096),
                thinking_kind: Some("enabled"),
                thinking_budget: Some(3_072),
            },
            // Room for the full budget: it is kept.
            SentCap {
                max_tokens: Some(40_000),
                thinking_kind: Some("enabled"),
                thinking_budget: Some(16_384),
            },
            // Under 2048 no budget of at least 1024 fits: thinking is off.
            SentCap {
                max_tokens: Some(1_500),
                thinking_kind: Some("disabled"),
                thinking_budget: None,
            },
            // No request: the default of the known ceiling, full budget.
            SentCap {
                max_tokens: Some(32_000),
                thinking_kind: Some("enabled"),
                thinking_budget: Some(16_384),
            },
        ]
    );
    // However the budget was planned, it stays below `max_tokens`.
    for body in &bodies {
        if let (Some(max_tokens), (_, Some(budget))) =
            (number(body, "max_tokens"), thinking_of(body))
        {
            assert!(budget < max_tokens, "{body:?}");
        }
    }
    Ok(())
}
