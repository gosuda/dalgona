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
            provider: None,
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
        endpoint: VertexEndpoint::Regional,
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
        endpoint: VertexEndpoint::Regional,
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
            endpoint: VertexEndpoint::Regional,
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
            endpoint: VertexEndpoint::Regional,
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
        provider: None,
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
        endpoint: VertexEndpoint::Regional,
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

#[test]
fn location_and_project_delimiters_never_reach_the_host_or_path() {
    let request = request();
    let thinking = plan();
    let build_with = |auth: VertexAuth<'_>| {
        build(&VertexRequest {
            request: &request,
            thinking: &thinking,
            auth,
            image_input: true,
            endpoint: VertexEndpoint::Regional,
        })
    };
    // Each value would turn the interpolated text into a sibling host, userinfo,
    // a port, a path escape, a query, a fragment, or a different byte string.
    for hostile in [
        "a.b", "a@b", "a:80", "a/b", "a?b", "a#b", "a\\b", "a%2fb", "a_b", "a\nb", "a\u{e9}b",
    ] {
        for auth in [
            VertexAuth::Bearer {
                access_token: "t",
                project: "proj-1",
                location: hostile,
            },
            VertexAuth::Bearer {
                access_token: "t",
                project: hostile,
                location: "us-central1",
            },
            VertexAuth::Express {
                api_key: "k",
                location: Some(hostile),
                explicit_location: true,
            },
            VertexAuth::Express {
                api_key: "k",
                location: Some(hostile),
                explicit_location: false,
            },
        ] {
            let error = build_with(auth).unwrap_err();
            assert!(
                matches!(error, ProviderError::InvalidRequest { .. }),
                "{hostile:?} must be refused, got {error:?}"
            );
        }
    }
    // The admitted side of the same boundary: letters of either case, digits,
    // and interior hyphens reach the host and the path unchanged.
    for (location, host) in [
        (
            "us-central1",
            "https://us-central1-aiplatform.googleapis.com",
        ),
        (
            "US-CENTRAL1",
            "https://US-CENTRAL1-aiplatform.googleapis.com",
        ),
        ("a-b-9", "https://a-b-9-aiplatform.googleapis.com"),
        ("x", "https://x-aiplatform.googleapis.com"),
    ] {
        let wire = build_with(VertexAuth::Bearer {
            access_token: "t",
            project: "Proj-9",
            location,
        })
        .unwrap();
        assert_eq!(wire.base_override.as_deref(), Some(host), "{location}");
        assert!(
            wire.path
                .starts_with(&format!("v1/projects/Proj-9/locations/{location}/")),
            "{}",
            wire.path
        );
    }
}

/// The typed endpoint-authority unit case: a configured provider base is
/// the endpoint authority (no regional override is allocated, and the path
/// still carries the captured project and location), while a defaulted row
/// keeps the regional location host.
#[test]
fn configured_provider_base_is_the_endpoint_authority() {
    let request = request();
    let thinking = plan();
    let configured = build(&VertexRequest {
        request: &request,
        thinking: &thinking,
        auth: VertexAuth::Bearer {
            access_token: "token",
            project: "proj-1",
            location: "us-central1",
        },
        image_input: false,
        endpoint: VertexEndpoint::ConfiguredProviderBase,
    })
    .unwrap();
    assert_eq!(configured.base_override, None);
    assert_eq!(
        configured.path,
        "v1/projects/proj-1/locations/us-central1/publishers/google/models/gemini-3-pro:streamGenerateContent?alt=sse"
    );
    assert_eq!(
        configured.headers,
        vec![("authorization", String::from("Bearer token"))]
    );
    assert!(configured.fallback.is_none());
    let defaulted = build(&VertexRequest {
        request: &request,
        thinking: &thinking,
        auth: VertexAuth::Bearer {
            access_token: "token",
            project: "proj-1",
            location: "us-central1",
        },
        image_input: false,
        endpoint: VertexEndpoint::Regional,
    })
    .unwrap();
    assert_eq!(
        defaulted.base_override.as_deref(),
        Some("https://us-central1-aiplatform.googleapis.com"),
    );
    assert!(defaulted.fallback.is_none());
}
