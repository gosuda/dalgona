//! Witnesses for the Antigravity surface: endpoints, user agent, session
//! envelope, wire profiles, compat policy, envelope shape, and validation
//! URL extraction.

use dal_core::{ContextItem, ModelRoute, Part, Purpose, RequestParams, ThinkingLevel};

use super::*;
use crate::thinking::{ThinkingPlan, WireThinking};

fn request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::GoogleAntigravity,
            model: "gemini-3-flash-agent".into(),
        },
        system: "Be brief.".into(),
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

fn envelope() -> Envelope {
    AntigravitySession::default().envelope(None, false)
}

#[test]
fn endpoints_keep_failover_for_known_bases() {
    let fallbacks = vec![
        String::from("https://daily-cloudcode-pa.googleapis.com"),
        String::from("https://daily-cloudcode-pa.sandbox.googleapis.com"),
    ];
    assert_eq!(endpoints(None), fallbacks);
    assert_eq!(endpoints(Some(DAILY_ENDPOINT)), fallbacks);
    assert_eq!(endpoints(Some(SANDBOX_ENDPOINT)), fallbacks);
    assert_eq!(
        endpoints(Some("https://daily-cloudcode-pa.googleapis.com///")),
        fallbacks
    );
    assert_eq!(
        endpoints(Some("https://proxy.example/cca")),
        vec![String::from("https://proxy.example/cca")]
    );
    assert_eq!(
        endpoints(Some("  https://proxy.example/cca  ")),
        vec![String::from("https://proxy.example/cca")]
    );
}

#[test]
fn user_agent_names_the_antigravity_client() {
    assert_eq!(
        user_agent("2.19.1"),
        "antigravity/hub/2.19.1 (aidev_client; os_type=darwin; arch=arm64; cl=963137146)"
    );
}

#[test]
fn session_envelope_steps_and_labels() {
    let mut session = AntigravitySession::default();
    session.seed_session_id(String::from("-123"));
    let first = session.envelope(Some("M16"), false);
    assert_eq!(first.session_id, "-123");
    let parts: Vec<&str> = first.request_id.split('/').collect();
    assert_eq!(parts.first(), Some(&"agent"));
    assert_eq!(parts.last(), Some(&"2"));
    let keys: Vec<&str> = first.labels.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "last_step_index",
            "model_enum",
            "trajectory_id",
            "used_claude",
            "used_claude_conservative"
        ]
    );
    assert_eq!(
        first.labels.first().map(|label| label.1.as_str()),
        Some("1")
    );
    assert_eq!(
        first.labels.get(1).map(|label| label.1.as_str()),
        Some("M16")
    );

    session.note_response_id(String::from("exec-9"));
    let second = session.envelope(None, true);
    assert_eq!(
        second.labels.first(),
        Some(&(String::from("last_execution_id"), String::from("exec-9")))
    );
    assert_eq!(
        second.labels.get(1).map(|label| label.1.as_str()),
        Some("2")
    );
    assert_eq!(second.session_id, "-123");
    assert!(second.request_id.ends_with("/3"), "{}", second.request_id);
}

#[test]
fn wire_profiles_match_the_backend_contract() {
    let profile = wire_profile("gemini-3-flash-agent").unwrap();
    assert_eq!(profile.max_output_tokens, 65536);
    let profile = wire_profile("claude-sonnet-4-6").unwrap();
    assert_eq!(profile.max_output_tokens, 64000);
    assert_eq!(profile.model_enum, None);
    assert!(wire_profile("gemini-9-unknown").is_none());
}

#[test]
fn compat_separates_claude_gemini_and_other_models() {
    let claude = compat("claude-sonnet-4-6");
    assert!(claude.part_ids);
    assert_eq!(claude.skip, SignaturePolicy::Never);
    assert!(claude.legacy_parameters);
    assert!(!claude.replay_thinking);
    assert_eq!(claude.tool_mode, ToolMode::Validated);

    let gemini = compat("gemini-3-flash-agent");
    assert!(!gemini.part_ids);
    assert_eq!(gemini.skip, SignaturePolicy::FirstCall);
    assert!(gemini.nested_tool_images);
    assert!(gemini.legacy_parameters);
    assert_eq!(gemini.tool_mode, ToolMode::Validated);

    let old = compat("gemini-2.5-flash");
    assert_eq!(old.skip, SignaturePolicy::Never);
    assert!(!old.nested_tool_images);
}

#[test]
fn build_sends_bearer_agent_envelope_and_validated_tools() {
    let request = request();
    let thinking = plan();
    let envelope = envelope();
    let wire = build(&AntigravityRequest {
        request: &request,
        thinking: &thinking,
        access_token: "token",
        project: "proj",
        wire_model: "gemini-3-flash-agent",
        client_version: "2.19.1",
        envelope: &envelope,
        claude_thinking: false,
        image_input: true,
    })
    .unwrap();
    assert_eq!(wire.path, "v1internal:streamGenerateContent?alt=sse");
    assert_eq!(
        wire.base_override.as_deref(),
        Some("https://daily-cloudcode-pa.googleapis.com")
    );
    assert_eq!(
        wire.headers,
        vec![("authorization", String::from("Bearer token"))]
    );
    assert_eq!(
        wire.user_agent.as_deref(),
        Some("antigravity/hub/2.19.1 (aidev_client; os_type=darwin; arch=arm64; cl=963137146)")
    );
    let body = String::from_utf8(wire.body).unwrap();
    for member in [
        "\"project\":\"proj\"",
        "\"requestId\":\"agent/",
        "\"model\":\"gemini-3-flash-agent\"",
        "\"userAgent\":\"antigravity\"",
        "\"requestType\":\"agent\"",
        "\"systemInstruction\":{\"role\":\"user\"",
        "\"maxOutputTokens\":65536",
        "\"thinkingLevel\":\"HIGH\"",
    ] {
        assert!(body.contains(member), "{body}");
    }
}

#[test]
fn claude_build_adds_the_thinking_beta_header() {
    let request = request();
    let thinking = plan();
    let envelope = envelope();
    let wire = build(&AntigravityRequest {
        request: &request,
        thinking: &thinking,
        access_token: "token",
        project: "proj",
        wire_model: "claude-sonnet-4-6",
        client_version: "2.19.1",
        envelope: &envelope,
        claude_thinking: true,
        image_input: true,
    })
    .unwrap();
    assert!(wire.headers.contains(&(
        "anthropic-beta",
        String::from("interleaved-thinking-2025-05-14")
    )));
}

#[test]
fn validation_url_requires_a_structured_validation_required_detail() {
    // A VALIDATION_REQUIRED detail carries the account-verification URL in
    // its metadata; an optional textual prefix before the JSON object is
    // tolerated.
    let good = "complete your verification {\"error\":{\"details\":[{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{\"validation_url\":\"https://accounts.example.com/verify/abc\"}}]}}";
    assert_eq!(
        validation_url(good),
        Some(String::from("https://accounts.example.com/verify/abc"))
    );
    // JSON escapes decode into an owned value the raw body never contains.
    let escaped = "{\"error\":{\"details\":[{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{\"validation_url\":\"https:\\/\\/accounts.example.com\\/verify\"}}]}}";
    assert_eq!(
        validation_url(escaped),
        Some(String::from("https://accounts.example.com/verify"))
    );
    // Plain quota and documentation links are not account verification.
    assert_eq!(validation_url("plain quota error"), None);
    assert_eq!(
        validation_url("see https://docs.example.com/quota for quota limits"),
        None
    );
    // A decoy link in the textual prefix must not win over the metadata URL.
    let decoy = "see https://decoy.example.org first {\"error\":{\"details\":[{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{\"validation_url\":\"https://accounts.example.com/verify/abc\"}}]}}";
    assert_eq!(
        validation_url(decoy),
        Some(String::from("https://accounts.example.com/verify/abc"))
    );
    // Any other reason, missing metadata, or an untrusted URL is rejected:
    // non-https scheme, empty host, embedded userinfo, and a decoded
    // terminal-control character never become verification.
    for body in [
        "{\"error\":{\"details\":[{\"reason\":\"OTHER\",\"metadata\":{\"validation_url\":\"https://accounts.example.com/verify/abc\"}}]}}",
        "{\"error\":{\"details\":[{\"reason\":\"VALIDATION_REQUIRED\"}]}}",
        "{\"error\":{\"details\":[{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{}}]}}",
        "{\"error\":{\"details\":[{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{\"validation_url\":\"http://accounts.example.com/verify\"}}]}}",
        "{\"error\":{\"details\":[{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{\"validation_url\":\"https://\"}}]}}",
        "{\"error\":{\"details\":[{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{\"validation_url\":\"https://user:pass@accounts.example.com/\"}}]}}",
        "{\"error\":{\"details\":[{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{\"validation_url\":\"https://accounts.example.com/\\u0001\"}}]}}",
    ] {
        assert_eq!(validation_url(body), None, "{body}");
    }
}
