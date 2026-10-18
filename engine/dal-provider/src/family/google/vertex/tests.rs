//! Witnesses for the Vertex AI surface: endpoint hosts, both credential
//! modes, the ambient-region global fallback, safety defaults, and input
//! validation.

use dal_core::{ContextItem, ModelRoute, Part, Purpose, RequestParams, ThinkingLevel};

use super::*;
use crate::thinking::{ThinkingPlan, WireThinking};

fn request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::GoogleVertex,
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
fn endpoint_hosts_resolve_per_location() {
    assert_eq!(endpoint_host("global"), "aiplatform.googleapis.com");
    assert_eq!(endpoint_host("eu"), "aiplatform.eu.rep.googleapis.com");
    assert_eq!(endpoint_host("us"), "aiplatform.us.rep.googleapis.com");
    assert_eq!(
        endpoint_host("asia-northeast1"),
        "asia-northeast1-aiplatform.googleapis.com"
    );
}

#[test]
fn bearer_build_targets_the_project_path() {
    let request = request();
    let thinking = plan();
    let wire = build(&VertexRequest {
        request: &request,
        thinking: &thinking,
        auth: VertexAuth::Bearer {
            access_token: "token",
            project: "proj-1",
            location: "us-central1",
        },
        image_input: true,
    })
    .unwrap();
    assert_eq!(
        wire.path,
        "v1/projects/proj-1/locations/us-central1/publishers/google/models/gemini-3-pro:streamGenerateContent?alt=sse"
    );
    assert_eq!(
        wire.base_override.as_deref(),
        Some("https://us-central1-aiplatform.googleapis.com")
    );
    assert_eq!(
        wire.headers,
        vec![("authorization", String::from("Bearer token"))]
    );
    assert!(wire.fallback.is_none());
    let body = String::from_utf8(wire.body).unwrap();
    assert!(body.contains("\"safetySettings\""), "{body}");
    assert!(body.contains("\"threshold\":\"OFF\""), "{body}");
    assert!(body.contains("HARM_CATEGORY_HARASSMENT"), "{body}");
}

#[test]
fn express_build_targets_the_publishers_path_with_global_fallback() {
    let request = request();
    let thinking = plan();
    let wire = build(&VertexRequest {
        request: &request,
        thinking: &thinking,
        auth: VertexAuth::Express {
            api_key: "key",
            location: Some("europe-west1"),
            explicit_location: false,
        },
        image_input: true,
    })
    .unwrap();
    assert_eq!(
        wire.path,
        "v1/publishers/google/models/gemini-3-pro:streamGenerateContent?alt=sse"
    );
    assert_eq!(
        wire.base_override.as_deref(),
        Some("https://europe-west1-aiplatform.googleapis.com")
    );
    assert_eq!(wire.headers, vec![("x-goog-api-key", String::from("key"))]);
    let fallback = wire.fallback.expect("ambient regions fall back to global");
    assert_eq!(
        fallback.base_override.as_deref(),
        Some("https://aiplatform.googleapis.com")
    );
    assert_eq!(fallback.path, wire.path);
}

#[test]
fn explicit_and_global_express_locations_do_not_fall_back() {
    let request = request();
    let thinking = plan();
    for (location, explicit) in [(Some("europe-west1"), true), (None, false)] {
        let wire = build(&VertexRequest {
            request: &request,
            thinking: &thinking,
            auth: VertexAuth::Express {
                api_key: "key",
                location,
                explicit_location: explicit,
            },
            image_input: true,
        })
        .unwrap();
        assert!(wire.fallback.is_none(), "{location:?}");
    }
}

#[test]
fn hostile_ids_are_refused_before_the_wire() {
    let base = request();
    let thinking = plan();
    for auth in [
        VertexAuth::Bearer {
            access_token: "t",
            project: "proj/../x",
            location: "us-central1",
        },
        VertexAuth::Bearer {
            access_token: "t",
            project: "proj-1",
            location: "a b",
        },
        VertexAuth::Express {
            api_key: "k",
            location: Some(""),
            explicit_location: true,
        },
    ] {
        let error = build(&VertexRequest {
            request: &base,
            thinking: &thinking,
            auth,
            image_input: true,
        })
        .unwrap_err();
        assert!(
            matches!(error, ProviderError::InvalidRequest { .. }),
            "{error:?}"
        );
    }
    let mut tuned = request();
    tuned.model = ModelRoute::Api {
        family: Family::GoogleVertex,
        model: "tunedModels/x".into(),
    };
    let error = build(&VertexRequest {
        request: &tuned,
        thinking: &plan(),
        auth: VertexAuth::Bearer {
            access_token: "t",
            project: "proj-1",
            location: "us-central1",
        },
        image_input: true,
    })
    .unwrap_err();
    assert!(
        matches!(error, ProviderError::InvalidRequest { .. }),
        "{error:?}"
    );
}

#[test]
fn compat_never_sends_ids_or_sentinels() {
    let policy = compat("gemini-3-pro");
    assert!(!policy.part_ids);
    assert_eq!(policy.skip, SignaturePolicy::Never);
    assert!(policy.nested_tool_images);
}
