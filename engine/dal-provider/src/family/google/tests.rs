//! Cross-surface witnesses for the Google family registry. Each surface
//! module pins its own `compat` table; this seam lowers one assistant turn
//! through every HTTP surface's public `build` and reads what reaches the
//! wire, so a surface wired to another surface's policy is caught even when
//! every per-surface table is still correct.

use dal_core::{
    AssistantPart, CallId, ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RawJson,
    ReplaySource, RequestParams, ThinkingLevel,
};
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use super::*;
use crate::thinking::{ThinkingPlan, WireThinking};

/// The bypass sentinel as the backend spells it on the wire.
const WIRE_SENTINEL: &str = "skip_thought_signature_validator";

fn thinking() -> ThinkingPlan {
    ThinkingPlan {
        level: ThinkingLevel::High,
        wire: WireThinking::OpenAi {
            effort: Some("high"),
        },
        temperature: None,
        notices: Vec::new(),
    }
}

/// One user turn, then one assistant message holding two unsigned tool
/// calls: the shape where every sentinel policy gives a different answer.
fn two_unsigned_calls(family: Family, model: &str) -> ModelRequest {
    let call = |id: &str| AssistantPart::ToolCall {
        call: CallId::new(id),
        name: "read".into(),
        args: RawJson::parse("{}").unwrap(),
        replay: None,
    };
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family,
            model: model.into(),
            provider: None,
        },
        system: "".into(),
        tools: Vec::new().into(),
        context: vec![
            ContextItem::User {
                parts: vec![Part::Text { text: "Hi".into() }],
            },
            ContextItem::Assistant {
                source: ReplaySource {
                    family,
                    model: model.into(),
                },
                replay: None,
                parts: vec![call("call_1"), call("call_2")],
            },
        ]
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

/// The `thoughtSignature` of every `functionCall` part in wire order, read
/// from the flat body's `contents` or the Cloud Code Assist envelope's
/// `request.contents`.
fn call_signatures(body: &[u8]) -> Vec<Option<String>> {
    let root: sonic_rs::Value = sonic_rs::from_slice(body).unwrap();
    let contents = if root["contents"].is_array() {
        &root["contents"]
    } else {
        &root["request"]["contents"]
    };
    let mut signatures = Vec::new();
    for content in contents.as_array().unwrap() {
        for part in content["parts"].as_array().unwrap() {
            if part["functionCall"].is_object() {
                signatures.push(part["thoughtSignature"].as_str().map(str::to_owned));
            }
        }
    }
    signatures
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "reads as one Option element beside the None and signed entries it is compared with"
)]
fn sentinel() -> Option<String> {
    Some(WIRE_SENTINEL.to_owned())
}

#[test]
fn every_surface_puts_its_own_sentinel_policy_on_the_wire() {
    let thinking = thinking();
    let envelope = antigravity::Envelope {
        session_id: String::from("-1"),
        request_id: String::from("agent/a/1/t/1"),
        labels: Vec::new(),
    };

    let direct = |model: &str| {
        let request = two_unsigned_calls(Family::GoogleGenerativeAi, model);
        let wire = gemini::build(&gemini::GeminiRequest {
            request: &request,
            thinking: &thinking,
            api_key: "key",
            image_input: true,
        })
        .unwrap();
        call_signatures(&wire.body)
    };
    let vertex = |auth: vertex::VertexAuth<'_>| {
        let request = two_unsigned_calls(Family::GoogleVertex, "gemini-3-pro");
        let wire = vertex::build(&vertex::VertexRequest {
            request: &request,
            thinking: &thinking,
            auth,
            image_input: true,
            endpoint: vertex::VertexEndpoint::Regional,
        })
        .unwrap();
        call_signatures(&wire.body)
    };
    let cca = |wire_model: &str| {
        let request = two_unsigned_calls(Family::GoogleAntigravity, wire_model);
        let wire = antigravity::build(&antigravity::AntigravityRequest {
            request: &request,
            thinking: &thinking,
            access_token: "token",
            project: "proj",
            wire_model,
            client_version: "2.19.1",
            envelope: &envelope,
            claude_thinking: false,
            image_input: true,
        })
        .unwrap();
        call_signatures(&wire.body)
    };

    // The direct API signs every unsigned call from Gemini 3 on and none
    // before it.
    assert_eq!(
        direct("gemini-3-pro"),
        vec![sentinel(), sentinel()],
        "direct gemini-3"
    );
    assert_eq!(
        direct("gemini-2.5-flash"),
        vec![None, None],
        "direct gemini-2.5"
    );
    // Vertex rejects the sentinel on every generation and both credentials.
    assert_eq!(
        vertex(vertex::VertexAuth::Bearer {
            access_token: "t",
            project: "proj-1",
            location: "us-central1",
        }),
        vec![None, None],
        "vertex bearer"
    );
    assert_eq!(
        vertex(vertex::VertexAuth::Express {
            api_key: "k",
            location: None,
            explicit_location: false,
        }),
        vec![None, None],
        "vertex express"
    );
    // Cloud Code Assist signs the first unsigned call of a Gemini 3 message
    // only, and never signs Claude or pre-3 routes.
    assert_eq!(
        cca("gemini-3-flash-agent"),
        vec![sentinel(), None],
        "antigravity gemini-3"
    );
    assert_eq!(
        cca("gemini-2.5-flash"),
        vec![None, None],
        "antigravity gemini-2.5"
    );
    assert_eq!(
        cca("claude-sonnet-4-6"),
        vec![None, None],
        "antigravity claude"
    );
}
