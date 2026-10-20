//! The Vertex AI family (`google-vertex`): `OAuth2` bearer or Express API key
//! over HTTPS, flat `GenerateContent` bodies with safety defaults, and SSE
//! decoding.
//!
//! Two credential modes:
//!
//! 1. **`OAuth2` bearer (standard).** `Authorization: Bearer <access_token>`
//!    against
//!    `https://{host}/v1/projects/{project}/locations/{location}/publishers/google/models/{id}:streamGenerateContent?alt=sse`.
//!    Token provenance is out-of-band (service-account file, gcloud user
//!    ADC, impersonation, or the metadata server, cached and refreshed
//!    skew-side); dal carries only the opaque token string. No
//!    `jsonwebtoken`/RS256 stack lives in dal.
//! 2. **Express Mode (API key).** `x-goog-api-key` against
//!    `https://{host}/v1/publishers/google/models/{id}:streamGenerateContent?alt=sse`
//!    (`GOOGLE_CLOUD_API_KEY`), with the regional/global host resolution
//!    below.
//!
//! Nothing is signed. Request lowering and stream decoding are the shared
//! `GenerateContent` grammar in [`super::shared`]; Vertex strips
//! function-part ids and never sends the signature bypass, which it rejects.
//! The model id is one path segment, so [`super::shared::request_body`]
//! refuses an id that is not a plain provider id before any wire exists.

use std::fmt;

use dal_core::{Family, ModelRequest};

use super::shared::{
    self, Compat, Dialect, FallbackTarget, GoogleRequest, GoogleWire, SignaturePolicy, ToolMode,
    generation_of,
};
use crate::{error::ProviderError, thinking::ThinkingPlan};

/// The API version segment of every Vertex AI path.
pub(crate) const API_VERSION: &str = "v1";

/// Resolves the Vertex AI endpoint host for a location: the global host, the
/// `eu` and `us` multi-region hosts, and one regional host for any other
/// location.
#[must_use]
pub(crate) fn endpoint_host(location: &str) -> String {
    match location {
        "global" => String::from("aiplatform.googleapis.com"),
        "eu" | "us" => format!("aiplatform.{location}.rep.googleapis.com"),
        location => format!("{location}-aiplatform.googleapis.com"),
    }
}

/// Whether the location is safe to interpolate into an endpoint host.
fn valid_location(location: &str) -> bool {
    !location.is_empty()
        && location
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Whether the project id is safe to interpolate into a request path.
fn valid_project(project: &str) -> bool {
    !project.is_empty()
        && project
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// How one Vertex AI request authenticates, resolved from the credential
/// store and the provider entry before lowering.
#[derive(Clone, Copy)]
pub(crate) enum VertexAuth<'a> {
    /// An `OAuth2` access token with its project and location; the token was
    /// minted and is refreshed outside dal.
    Bearer {
        /// The current access token.
        access_token: &'a str,
        /// The Google Cloud project id.
        project: &'a str,
        /// The model location.
        location: &'a str,
    },
    /// A Vertex Express API key.
    Express {
        /// The API key.
        api_key: &'a str,
        /// The model location; `None` is the global endpoint.
        location: Option<&'a str>,
        /// An explicit location is a residency choice: a 404 surfaces
        /// instead of falling back to the global endpoint.
        explicit_location: bool,
    },
}

impl fmt::Debug for VertexAuth<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer {
                project, location, ..
            } => formatter
                .debug_struct("Bearer")
                .field("access_token", &"<redacted>")
                .field("project", project)
                .field("location", location)
                .finish(),
            Self::Express {
                location,
                explicit_location,
                ..
            } => formatter
                .debug_struct("Express")
                .field("api_key", &"<redacted>")
                .field("location", location)
                .field("explicit_location", explicit_location)
                .finish(),
        }
    }
}

/// Everything one Vertex AI request needs besides the neutral request.
#[derive(Clone, Copy)]
pub(crate) struct VertexRequest<'a> {
    /// The neutral request; its route id is the wire model name.
    pub(crate) request: &'a ModelRequest,
    /// The thinking fragment from the composed plan.
    pub(crate) thinking: &'a ThinkingPlan,
    /// How the request authenticates.
    pub(crate) auth: VertexAuth<'a>,
    /// Whether the catalog entry accepts image input.
    pub(crate) image_input: bool,
}

/// The wire policy for one Vertex AI model id: no function-part ids and no
/// signature bypass on any generation; Gemini 3+ still nests tool images.
#[must_use]
pub(crate) fn compat(model: &str) -> Compat {
    Compat {
        part_ids: false,
        skip: SignaturePolicy::Never,
        nested_tool_images: generation_of(model).is_three_or_later(),
        legacy_parameters: false,
        replay_thinking: true,
        tool_mode: ToolMode::Auto,
    }
}

/// Lowers one request to its path, headers, and body.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] for a route of another family, an
/// unusable project, location, or model id, a blob part, or an
/// unserializable body.
pub(crate) fn build(input: &VertexRequest<'_>) -> Result<GoogleWire, ProviderError> {
    let model = shared::route_model(input.request, Family::GoogleVertex)?;
    let body = shared::request_body(&GoogleRequest {
        family: Family::GoogleVertex,
        request: input.request,
        thinking: input.thinking,
        compat: compat(model),
        image_input: input.image_input,
        dialect: Dialect::Flat { safety_off: true },
    })?;
    match input.auth {
        VertexAuth::Bearer {
            access_token,
            project,
            location,
        } => {
            if !valid_project(project) {
                return Err(ProviderError::InvalidRequest {
                    message: format!("vertex project id {project:?} is not usable"),
                });
            }
            if !valid_location(location) {
                return Err(ProviderError::InvalidRequest {
                    message: format!("vertex location {location:?} is not usable"),
                });
            }
            Ok(GoogleWire {
                path: format!(
                    "{API_VERSION}/projects/{project}/locations/{location}/publishers/google/models/{model}:streamGenerateContent?alt=sse"
                ),
                base_override: Some(format!("https://{}", endpoint_host(location))),
                headers: vec![("authorization", format!("Bearer {access_token}"))],
                user_agent: None,
                body,
                fallback: None,
            })
        }
        VertexAuth::Express {
            api_key,
            location,
            explicit_location,
        } => {
            // An ambient env-derived region falls back to the global
            // endpoint so a stray location never breaks a previously-working
            // global-only request; an explicit region is honored and a 404
            // surfaces.
            let location = location.unwrap_or("global");
            if !valid_location(location) {
                return Err(ProviderError::InvalidRequest {
                    message: format!("vertex location {location:?} is not usable"),
                });
            }
            let path = format!(
                "{API_VERSION}/publishers/google/models/{model}:streamGenerateContent?alt=sse"
            );
            let host = endpoint_host(location);
            let regional = host != "aiplatform.googleapis.com";
            Ok(GoogleWire {
                path,
                base_override: Some(format!("https://{host}")),
                headers: vec![(super::gemini::API_KEY_HEADER, api_key.to_owned())],
                user_agent: None,
                body,
                fallback: (!explicit_location && regional).then(|| FallbackTarget {
                    base_override: Some(String::from("https://aiplatform.googleapis.com")),
                    path: format!(
                        "{API_VERSION}/publishers/google/models/{model}:streamGenerateContent?alt=sse"
                    ),
                }),
            })
        }
    }
}

#[cfg(test)]
mod tests;
