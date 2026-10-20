//! The direct Gemini API family (`google-generative-ai`): API-key auth over
//! HTTPS, flat `GenerateContent` bodies, and SSE decoding.
//!
//! The credential is an API key in the **`x-goog-api-key`** header, resolved
//! from `GEMINI_API_KEY` or the stored credential and posted to
//! `{base}/models/{id}:streamGenerateContent?alt=sse`. Nothing is signed.
//! The model id is one path segment, so a route whose id is not a plain
//! provider id (see [`super::shared::plain_model_id`]) is refused before any
//! wire exists. Request lowering and stream decoding are the shared
//! `GenerateContent` grammar in [`super::shared`].

use dal_core::{Family, ModelRequest};

use super::shared::{
    self, Compat, Dialect, Generation, GoogleRequest, GoogleWire, SignaturePolicy, ToolMode,
    generation_of,
};
use crate::{error::ProviderError, thinking::ThinkingPlan};

/// The API-key header: the only credential this surface carries. The key
/// itself resolves from `GEMINI_API_KEY` in the join's credential wiring.
pub(crate) const API_KEY_HEADER: &str = "x-goog-api-key";

/// Everything one direct-API request needs besides the neutral request.
#[derive(Clone, Copy)]
pub(crate) struct GeminiRequest<'a> {
    /// The neutral request; its route id is the wire model name.
    pub(crate) request: &'a ModelRequest,
    /// The thinking fragment from the composed plan.
    pub(crate) thinking: &'a ThinkingPlan,
    /// The API key, resolved by the caller from the credential store.
    pub(crate) api_key: &'a str,
    /// Whether the catalog entry accepts image input.
    pub(crate) image_input: bool,
}

/// The wire policy for one direct-API model id: Gemini 3+ carries
/// function-part ids, the bypass sentinel on every unsigned call, and nested
/// tool images; earlier generations carry none of the three.
#[must_use]
pub(crate) fn compat(model: &str) -> Compat {
    compat_for(generation_of(model))
}

fn compat_for(generation: Generation) -> Compat {
    let three = generation.is_three_or_later();
    Compat {
        part_ids: three,
        skip: if three {
            SignaturePolicy::Always
        } else {
            SignaturePolicy::Never
        },
        nested_tool_images: three,
        legacy_parameters: false,
        replay_thinking: true,
        tool_mode: ToolMode::Auto,
    }
}

/// Lowers one request to its path, headers, and body.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] for a route of another family, a model
/// id that is not a plain provider id, a blob part, or an unserializable
/// body.
pub(crate) fn build(input: &GeminiRequest<'_>) -> Result<GoogleWire, ProviderError> {
    let model = shared::route_model(input.request, Family::GoogleGenerativeAi)?;
    let body = shared::request_body(&GoogleRequest {
        family: Family::GoogleGenerativeAi,
        request: input.request,
        thinking: input.thinking,
        compat: compat(model),
        image_input: input.image_input,
        dialect: Dialect::Flat { safety_off: false },
    })?;
    Ok(GoogleWire {
        path: format!("models/{model}:streamGenerateContent?alt=sse"),
        base_override: None,
        headers: vec![(API_KEY_HEADER, input.api_key.to_owned())],
        user_agent: None,
        body,
        fallback: None,
    })
}

#[cfg(test)]
mod tests;
