//! The Devin family: Codeium Cascade over Connect framing.
//!
//! Devin shares the Connect length-prefixed framing with Cursor but is a
//! distinct protocol family: `exa.*` protobuf schemas, its own discovery
//! (`GetCliModelConfigs`), its own session lifecycle (`GetUserJwt`, then
//! `AssignModel` on router models, then one `GetChatMessage` stream per
//! turn), and its own delta vocabulary. No message type is shared with any
//! future Cursor module; only the generic framing shape coincides, and even
//! that lives here.
//!
//! Ground truth is the pinned reference at `a4dc3302`
//! (`packages/ai/src/providers/devin.ts`, `connect-frame.ts`,
//! `connect-error-detail.ts`, `utils/tool-call-arguments.ts`,
//! `providers/devin/proto/exa/`, `catalog/src/{wire,discovery}/devin*.ts`,
//! `ai/src/usage/devin.ts`): session creation, delta and streaming events,
//! tool exchange, cancellation, and exactly one terminal outcome. Where the
//! reference evolved between revisions, this module follows the pin: CLI
//! identity `devin-cli`/`chisel` at `3000.11.3`, the two-key auth ladder
//! (session token first, raw key on 401), completion config
//! `400/1/40/fround(0.95)` with caller stop sequences only, bare-UUID
//! assistant ids, and 40-char sanitized tool-call ids.
//!
//! Layout: [`auth`] holds the CLI sign-in exchange, [`proto`] the prost
//! message transcriptions, [`connect`] the Connect framing, [`history`] the context lowering, [`request`] the
//! RPC body builders, [`decode`] the response decoder and discovery
//! normalization, [`terminal`] the exactly-one-terminal latch. This module
//! owns no transport: the join sends the bodies over `http::send` in the
//! order auth, optional assignment, chat, and drives [`decode_chunks`]
//! over the chat response body inside an `EventStream`.
//!
//! [`decode_chunks`]: decode::decode_chunks

pub(crate) mod auth;
pub(crate) mod connect;
pub(crate) mod decode;
pub(crate) mod history;
pub(crate) mod proto;
pub(crate) mod request;
pub(crate) mod terminal;

#[cfg(test)]
mod tests;

/// Streaming chat RPC path, relative to the base URL.
pub(crate) const PATH_CHAT: &str = "/exa.api_server_pb.ApiServerService/GetChatMessage";
/// Router assignment RPC path, relative to the base URL.
pub(crate) const PATH_ASSIGN: &str = "/exa.api_server_pb.ApiServerService/AssignModel";
/// Session-token exchange RPC path, relative to the base URL.
pub(crate) const PATH_AUTH: &str = "/exa.auth_pb.AuthService/GetUserJwt";
/// Model discovery RPC path, relative to the base URL.
pub(crate) const PATH_DISCOVERY: &str = "/exa.api_server_pb.ApiServerService/GetCliModelConfigs";
