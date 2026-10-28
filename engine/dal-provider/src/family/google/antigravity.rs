//! The Antigravity family (`google-antigravity`): OAuth Bearer to the Cloud
//! Code Assist endpoints, with the CCA request envelope and per-surface
//! user agent.
//!
//! The credential class matches Vertex OAuth — an opaque
//! `Authorization: Bearer <access_token>` attached per request, minted and
//! refreshed outside dal — but the endpoint and envelope differ:
//!
//! - Default order `https://daily-cloudcode-pa.googleapis.com`, then the
//!   sandbox `https://daily-cloudcode-pa.sandbox.googleapis.com`; a
//!   configured base that is neither is used alone.
//! - Path `/v1internal:streamGenerateContent?alt=sse`.
//! - Per-surface `User-Agent`: `antigravity/hub/<version> (aidev_client; ...)`
//!   instead of the Gemini CLI headers.
//! - Envelope `{project, requestId, request, model, userAgent: "antigravity",
//!   requestType: "agent"}` with session labels, and `VALIDATED` tool mode.
//!
//! Nothing is signed. Antigravity keeps its own family rather than sharing
//! `google-gemini-cli`: shared CCA framing is not shared identity.

use dal_core::{Family, ModelRequest};
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use super::shared::{
    self, Compat, Dialect, GoogleRequest, GoogleWire, SignaturePolicy, ToolMode, generation_of,
    is_claude_model,
};
use crate::{error::ProviderError, thinking::ThinkingPlan};

/// The Antigravity daily endpoint, first in the failover order.
pub(crate) const DAILY_ENDPOINT: &str = "https://daily-cloudcode-pa.googleapis.com";

/// The Antigravity sandbox endpoint, last in the failover order.
pub(crate) const SANDBOX_ENDPOINT: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";

/// The Antigravity client version the `User-Agent` carries, captured from the
/// real client (the backend gates on the version, like Devin's pinned CLI
/// identity). The request path and the sign-in's project discovery both name
/// this one value.
pub(crate) const CLIENT_VERSION: &str = "2.19.1";

/// The unchanged `cl` token the backend expects beside the client version: it
/// stays fixed while the version moves.
const USER_AGENT_CL: &str = "963137146";

/// Builds the Antigravity `User-Agent` header value for a client version:
/// `antigravity/hub/<version> (aidev_client; os_type=darwin; arch=arm64;
/// cl=<cl>)`. The os/arch pair is fixed, independent of the host platform.
#[must_use]
pub(crate) fn user_agent(version: &str) -> String {
    format!(
        "antigravity/hub/{version} (aidev_client; os_type=darwin; arch=arm64; cl={USER_AGENT_CL})"
    )
}

/// The candidate endpoints for one request, in try order: the daily endpoint
/// then the sandbox endpoint, unless a configured base is neither of them,
/// in which case that base is the only target. The configured base is
/// trimmed first, so padding never reaches the wire; a trimmed base naming a
/// known endpoint (trailing slashes tolerated) keeps the default order.
/// Failover stays safe only before the first streamed event; the transport
/// join owns that boundary.
#[must_use]
pub(crate) fn endpoints(configured: Option<&str>) -> Vec<String> {
    let fallbacks = || vec![DAILY_ENDPOINT.to_owned(), SANDBOX_ENDPOINT.to_owned()];
    match configured {
        Some(base) => {
            let trimmed = base.trim();
            let normalized = trimmed.trim_end_matches('/');
            if normalized == DAILY_ENDPOINT || normalized == SANDBOX_ENDPOINT {
                fallbacks()
            } else {
                vec![trimmed.to_owned()]
            }
        }
        None => fallbacks(),
    }
}

/// The per-wire-id Antigravity request constants: the opaque
/// `labels.model_enum` telemetry token and the fixed
/// `generationConfig.maxOutputTokens` the backend enforces regardless of the
/// thinking budget. Keyed by the routed upstream wire id.
pub(crate) struct WireProfile {
    /// The opaque `labels.model_enum` token, when the wire id carries one.
    pub(crate) model_enum: Option<&'static str>,
    /// The fixed output cap for the wire id.
    pub(crate) max_output_tokens: u32,
}

/// Looks up the wire profile for a routed upstream wire id.
#[must_use]
pub(crate) fn wire_profile(wire_model: &str) -> Option<&'static WireProfile> {
    const GEMINI_35_FLASH_EXTRA_LOW: WireProfile = WireProfile {
        model_enum: Some("MODEL_PLACEHOLDER_M187"),
        max_output_tokens: 65536,
    };
    const GEMINI_35_FLASH_LOW: WireProfile = WireProfile {
        model_enum: Some("MODEL_PLACEHOLDER_M20"),
        max_output_tokens: 65536,
    };
    const GEMINI_3_FLASH_AGENT: WireProfile = WireProfile {
        model_enum: Some("MODEL_PLACEHOLDER_M132"),
        max_output_tokens: 65536,
    };
    const GEMINI_31_PRO_LOW: WireProfile = WireProfile {
        model_enum: Some("MODEL_PLACEHOLDER_M36"),
        max_output_tokens: 65535,
    };
    const GEMINI_PRO_AGENT: WireProfile = WireProfile {
        model_enum: Some("MODEL_PLACEHOLDER_M16"),
        max_output_tokens: 65535,
    };
    const CLAUDE_SONNET_4_6: WireProfile = WireProfile {
        model_enum: None,
        max_output_tokens: 64000,
    };
    const CLAUDE_OPUS_4_6_THINKING: WireProfile = WireProfile {
        model_enum: None,
        max_output_tokens: 64000,
    };
    Some(match wire_model {
        "gemini-3.5-flash-extra-low" => &GEMINI_35_FLASH_EXTRA_LOW,
        "gemini-3.5-flash-low" => &GEMINI_35_FLASH_LOW,
        "gemini-3-flash-agent" => &GEMINI_3_FLASH_AGENT,
        "gemini-3.1-pro-low" => &GEMINI_31_PRO_LOW,
        "gemini-pro-agent" => &GEMINI_PRO_AGENT,
        // Claude on `daily-cloudcode-pa` rejects `maxOutputTokens > 64000`
        // with a 400; the model_enum label is untracked for these ids and
        // the backend does not require it.
        "claude-sonnet-4-6" => &CLAUDE_SONNET_4_6,
        "claude-opus-4-6-thinking" => &CLAUDE_OPUS_4_6_THINKING,
        _ => return None,
    })
}

/// The per-conversation Antigravity session: the agent, trajectory, and
/// session ids plus the step counter behind the request envelope. The join
/// owns one per session and seeds it from the session id; without seeding
/// the ids are fresh per process.
#[derive(Clone, Debug, Default)]
pub(crate) struct AntigravitySession {
    agent_id: Option<String>,
    trajectory_id: Option<String>,
    session_id: Option<String>,
    step_index: Option<u64>,
    last_execution_id: Option<String>,
}

/// One request's envelope values: `requestId`, `sessionId`, and `labels`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Envelope {
    /// The CCA `sessionId` member.
    pub(crate) session_id: String,
    /// The envelope `requestId` member:
    /// `agent/<agentId>/<ms>/<trajectoryId>/<step>`.
    pub(crate) request_id: String,
    /// The `labels` members, in wire order.
    pub(crate) labels: Vec<(String, String)>,
}

impl AntigravitySession {
    /// Seeds the CCA session id (a fresh process derives a random decimal
    /// id; the join seeds a stable one per dal session).
    pub(crate) fn seed_session_id(&mut self, session_id: String) {
        if self.session_id.is_none() {
            self.session_id = Some(session_id);
        }
    }

    /// Records the last response id for the next request's
    /// `labels.last_execution_id`.
    pub(crate) fn note_response_id(&mut self, response_id: String) {
        self.last_execution_id = Some(response_id);
    }

    /// Advances the per-conversation state and returns one request's
    /// envelope values (mirrors the real client: the step starts at 2 and
    /// `last_step_index` trails the request step by one).
    pub(crate) fn envelope(&mut self, model_enum: Option<&str>, used_claude: bool) -> Envelope {
        let agent_id = self
            .agent_id
            .get_or_insert_with(|| uuid::Uuid::new_v4().simple().to_string())
            .clone();
        let trajectory_id = self
            .trajectory_id
            .get_or_insert_with(|| uuid::Uuid::new_v4().simple().to_string())
            .clone();
        let session_id = self
            .session_id
            .get_or_insert_with(random_session_id)
            .clone();
        let step = self.step_index.map_or(2, |step| step + 1);
        self.step_index = Some(step);
        let mut labels = Vec::with_capacity(5);
        if let Some(last) = &self.last_execution_id {
            labels.push((String::from("last_execution_id"), last.clone()));
        }
        labels.push((String::from("last_step_index"), (step - 1).to_string()));
        if let Some(model_enum) = model_enum {
            labels.push((String::from("model_enum"), model_enum.to_owned()));
        }
        labels.push((String::from("trajectory_id"), trajectory_id.clone()));
        labels.push((String::from("used_claude"), used_claude.to_string()));
        labels.push((
            String::from("used_claude_conservative"),
            used_claude.to_string(),
        ));
        Envelope {
            session_id,
            request_id: format!(
                "agent/{agent_id}/{}/{trajectory_id}/{step}",
                ms_since_epoch()
            ),
            labels,
        }
    }
}

/// A fresh random decimal session id (`-<63-bit int>`, the reference
/// `randomSignedDecimalSessionId` shape).
fn random_session_id() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut value = 0u64;
    for byte in bytes {
        value = (value << 8) | u64::from(byte);
    }
    value &= (1 << 63) - 1;
    format!("-{value}")
}

/// Wall-clock milliseconds for the envelope `requestId`: an id component,
/// not a duration, so the system clock (not the monotonic clock) is the
/// right source.
fn ms_since_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

/// The wire policy for one Antigravity wire id. Claude routes take
/// function-part ids and the legacy `parameters` schema, drop unsigned
/// thinking, and never take the bypass sentinel; Gemini 3+ takes the
/// sentinel on the first unsigned call only and nests tool images; other
/// models take parts without either.
#[must_use]
pub(crate) fn compat(wire_model: &str) -> Compat {
    if is_claude_model(wire_model) {
        return Compat {
            part_ids: true,
            skip: SignaturePolicy::Never,
            nested_tool_images: false,
            legacy_parameters: true,
            replay_thinking: false,
            tool_mode: ToolMode::Validated,
        };
    }
    if wire_model.starts_with("gemini-") {
        let three = generation_of(wire_model).is_three_or_later();
        return Compat {
            part_ids: false,
            skip: if three {
                SignaturePolicy::FirstCall
            } else {
                SignaturePolicy::Never
            },
            nested_tool_images: three,
            legacy_parameters: true,
            replay_thinking: true,
            tool_mode: ToolMode::Validated,
        };
    }
    Compat {
        part_ids: true,
        skip: SignaturePolicy::Never,
        nested_tool_images: false,
        legacy_parameters: true,
        replay_thinking: true,
        tool_mode: ToolMode::Validated,
    }
}

/// Everything one Antigravity request needs besides the neutral request.
pub(crate) struct AntigravityRequest<'a> {
    /// The neutral request; the route id selects the catalog row while
    /// `wire_model` names the upstream model.
    pub(crate) request: &'a ModelRequest,
    /// The thinking fragment from the composed plan.
    pub(crate) thinking: &'a ThinkingPlan,
    /// The OAuth access token, minted and refreshed outside dal.
    pub(crate) access_token: &'a str,
    /// The Google Cloud project id the request names.
    pub(crate) project: &'a str,
    /// The routed upstream wire id.
    pub(crate) wire_model: &'a str,
    /// The client version for the `User-Agent` header.
    pub(crate) client_version: &'a str,
    /// The envelope values from the session.
    pub(crate) envelope: &'a Envelope,
    /// Whether the request carries Claude thinking: adds the
    /// `anthropic-beta: interleaved-thinking-2025-05-14` header.
    pub(crate) claude_thinking: bool,
    /// Whether the catalog entry accepts image input.
    pub(crate) image_input: bool,
}

/// Lowers one request to its path, headers, and body.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] for a route of another family, a blob
/// part, or an unserializable body.
pub(crate) fn build(input: &AntigravityRequest<'_>) -> Result<GoogleWire, ProviderError> {
    let profile = wire_profile(input.wire_model);
    let mut headers = vec![("authorization", format!("Bearer {}", input.access_token))];
    if input.claude_thinking {
        headers.push((
            "anthropic-beta",
            String::from("interleaved-thinking-2025-05-14"),
        ));
    }
    let body = shared::request_body(&GoogleRequest {
        family: Family::GoogleAntigravity,
        request: input.request,
        thinking: input.thinking,
        compat: compat(input.wire_model),
        image_input: input.image_input,
        dialect: Dialect::CloudCodeAssist {
            project: input.project,
            session_id: &input.envelope.session_id,
            request_id: &input.envelope.request_id,
            user_agent: "antigravity",
            labels: &input.envelope.labels,
            max_output_tokens: profile.map(|profile| profile.max_output_tokens),
        },
    })?;
    Ok(GoogleWire {
        path: super::CCA_STREAM_PATH.to_owned(),
        // The caller selects the endpoint from `endpoints()`; the default
        // target is the daily endpoint.
        base_override: Some(DAILY_ENDPOINT.to_owned()),
        headers,
        user_agent: Some(user_agent(input.client_version)),
        body,
        fallback: None,
    })
}

/// Extracts the account-validation URL from a validation-required error
/// body, when one is present: an optional textual prefix before the first
/// JSON object, then the `details` entry whose reason is exactly
/// `VALIDATION_REQUIRED` with the URL in its `metadata.validation_url`;
/// unknown fields are tolerated.
/// The URL is owned because JSON escapes decode on parse, so the value need
/// not appear verbatim in the body. The join renders it inside its fixed
/// account-verification message; plain quota or documentation links are
/// never account verification. The URL is untrusted text and never fetched.
#[must_use]
pub(crate) fn validation_url(body: &str) -> Option<String> {
    if !body.contains("VALIDATION_REQUIRED") {
        return None;
    }
    let json = body.find('{').map(|start| &body[start..])?;
    let parsed: sonic_rs::Value = sonic_rs::from_str(json).ok()?;
    let details = parsed.get("error")?.get("details")?.as_array()?;
    let entry = details.iter().find(|detail| {
        detail.get("reason").and_then(|reason| reason.as_str()) == Some("VALIDATION_REQUIRED")
    })?;
    let raw = entry.get("metadata")?.get("validation_url")?.as_str()?;
    valid_verification_url(raw)
}

/// Whether a decoded `validation_url` is safe to render: an `https` URL with
/// a nonempty host, no embedded username or password, and no control
/// characters. Any host is accepted: the contract names no allowlist, and
/// the URL is rendered, never fetched.
fn valid_verification_url(raw: &str) -> Option<String> {
    let url = raw.trim();
    if url.is_empty() || url.chars().any(char::is_control) {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return None;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    Some(url.to_owned())
}

#[cfg(test)]
mod tests;
