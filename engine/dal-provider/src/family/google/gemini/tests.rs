//! Witnesses for the direct Gemini API surface: headers, path, compat
//! policy, and route validation. Decoding is the shared grammar, witnessed
//! in `super::shared::tests`.

use dal_core::{ContextItem, ModelRoute, Part, Purpose, RequestParams, ThinkingLevel};

use super::*;
use crate::thinking::{ThinkingPlan, WireThinking};

fn request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::GoogleGenerativeAi,
            model: "gemini-3-pro".into(),
        },
        system: "".into(),
        tools: Vec::new().into(),
        context: vec![ContextItem::User {
            parts: vec![Part::Text { text: "Hi".into() }],
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

fn plan() -> ThinkingPlan {
    ThinkingPlan {
        level: ThinkingLevel::High,
        wire: WireThinking::OpenAi {
            effort: Some("high"),
        },
        temperature: None,
        notices: Vec::new(),
    }
}

#[test]
fn build_sends_the_key_header_and_model_path() {
    let request = request();
    let thinking = plan();
    let wire = build(&GeminiRequest {
        request: &request,
        thinking: &thinking,
        api_key: "secret-key",
        image_input: true,
    })
    .unwrap();
    assert_eq!(
        wire.path,
        "models/gemini-3-pro:streamGenerateContent?alt=sse"
    );
    assert_eq!(wire.base_override, None);
    assert_eq!(wire.user_agent, None);
    assert!(wire.fallback.is_none());
    assert_eq!(
        wire.headers,
        vec![(API_KEY_HEADER, String::from("secret-key"))]
    );
    let body = String::from_utf8(wire.body).unwrap();
    assert!(body.contains("\"contents\""), "{body}");
}

#[test]
fn compat_gates_generation_three_features() {
    let three = compat("gemini-3-pro");
    assert!(three.part_ids);
    assert_eq!(three.skip, SignaturePolicy::Always);
    assert!(three.nested_tool_images);

    let two = compat("gemini-2.5-flash");
    assert!(!two.part_ids);
    assert_eq!(two.skip, SignaturePolicy::Never);
    assert!(!two.nested_tool_images);
}

#[test]
fn model_ids_that_leave_the_path_are_refused() {
    let thinking = plan();
    for model in [
        "",
        "tunedModels/x",
        "models/../secret",
        "gemini-3-pro?alt=json",
        "gemini-3-pro#fragment",
        "gemini 3 pro",
        "gemini/3/pro",
    ] {
        let mut request = request();
        request.model = ModelRoute::Api {
            family: Family::GoogleGenerativeAi,
            model: model.into(),
        };
        let error = build(&GeminiRequest {
            request: &request,
            thinking: &thinking,
            api_key: "secret-key",
            image_input: true,
        })
        .unwrap_err();
        assert!(
            matches!(error, ProviderError::InvalidRequest { .. }),
            "{model:?}: {error:?}"
        );
    }
}

#[test]
fn wrong_family_route_is_refused() {
    let mut request = request();
    request.model = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-6".into(),
    };
    let thinking = plan();
    let error = build(&GeminiRequest {
        request: &request,
        thinking: &thinking,
        api_key: "secret-key",
        image_input: true,
    })
    .unwrap_err();
    assert!(matches!(error, ProviderError::InvalidRequest { .. }));
}
