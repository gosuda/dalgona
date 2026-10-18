//! The Google families: direct Gemini API, Vertex AI, Gemini CLI, and
//! Antigravity, behind one shared `GenerateContent` lowering and stream decoding.
//!
//! The wire surfaces share the `GenerateContent` grammar (`contents`,
//! `systemInstruction`, `tools`, `thinkingConfig`) but differ in transport and
//! trust boundaries:
//!
//! | Surface | Family id | Transport | Credential |
//! |---|---|---|---|
//! | Direct Gemini API | `google-generative-ai` | HTTPS SSE | API key in `x-goog-api-key` |
//! | Vertex AI | `google-vertex` | HTTPS SSE | OAuth `Authorization: Bearer`, or Express `x-goog-api-key` |
//! | Gemini CLI | `google-gemini-cli` | local subprocess | none; the CLI owns its own store |
//! | Antigravity | `google-antigravity` | HTTPS SSE | OAuth `Authorization: Bearer` |
//!
//! No request is ever signed: no request signing, no signing dependency, and
//! no mTLS or attestation exists on any of the four surfaces.
//!
//! [`shared::request_body`] lowers one [`ModelRequest`] and its composed
//! [`ThinkingPlan`] to the exact JSON body of `POST <base>/models/{id}:
//! streamGenerateContent?alt=sse` (Gemini/Vertex) or the Cloud Code Assist
//! envelope (`{project, model, request, requestType}`, Antigravity only;
//! the Gemini CLI surface is a credential-less local subprocess, never an
//! HTTP client). Tool-call arguments are written from their [`RawJson`] text,
//! never re-encoded; tool schemas are projected onto the subset the wire
//! accepts (`schema::project`). The Gemini 3 `thinkingLevel` spelling
//! and the `thoughtSignature` fields travel byte-exact through the decoders
//! ([`shared::GeminiDecoder`]) so a signed signature reaches the next turn's
//! replay bound for the same family and model.
//!
//! Family ids follow the pinned registry spellings (`google-generative-ai`,
//! `google-vertex`, `google-gemini-cli`, `google-antigravity`), matching the
//! pinned registries: oh-my-pi `api-registry.ts:27-29` and the senpi api ids.
//! Antigravity keeps its own variant rather than sharing `google-gemini-cli`:
//! shared CCA HTTP framing is not shared identity (the Cursor/Devin rule).

pub(crate) mod antigravity;
pub(crate) mod gemini;
pub(crate) mod gemini_cli;
pub(crate) mod schema;
pub(crate) mod shared;
pub(crate) mod vertex;

/// The Cloud Code Assist stream path relative to the CCA endpoint; the
/// Antigravity surface POSTs here. The direct Gemini API and Vertex AI use
/// `models/{model}:streamGenerateContent?alt=sse` and the Vertex project
/// paths their modules build.
pub(crate) const CCA_STREAM_PATH: &str = "v1internal:streamGenerateContent?alt=sse";

#[cfg(test)]
mod tests;
