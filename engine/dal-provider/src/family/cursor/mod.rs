//! The Cursor agent family (`cursor`): Connect-framed `Run` codec.
//!
//! This module owns the wire codec for Cursor's `agent.v1.AgentService/Run`
//! RPC: the 5-byte Connect envelope ([`frame`]), the `agent.v1` protobuf
//! messages as `prost` 0.14.4 derive structs with no `protoc` step
//! ([`proto`]), request lowering from [`ModelRequest`](dal_core::ModelRequest)
//! ([`lower`]), and the server-message state machine that emits the neutral
//! [`StreamEvent`](crate::stream::StreamEvent) grammar ([`decode`]). It is
//! transport-agnostic: it consumes response bytes and produces request bytes,
//! so the HTTP/2 transport carries it unchanged (see the transport note
//! below).
//!
//! # Coded-against surface
//!
//! * oh-my-pi at `a4dc3302` (verified on disk under
//!   `.references/oh-my-pi`): `packages/ai/src/providers/cursor.ts` (stream
//!   pump, `turnEnded` terminal, checkpoint capture, `ErrorDetails` tables),
//!   `packages/ai/src/providers/connect-frame.ts` (envelope),
//!   `packages/ai/src/providers/cursor/interaction-query.ts` (query replies),
//!   `packages/catalog/src/wire/cursor.ts` (paths, headers, client version),
//!   `packages/catalog/src/discovery/cursor-proto.ts` (field numbers).
//!   The worktree has since moved to `a536558d`; `cursor-proto.ts`,
//!   `wire/cursor.ts`, `connect-frame.ts`, and `interaction-query.ts` are
//!   byte-identical between the pin and HEAD, and the `cursor.ts` drift
//!   (approval probe unchanged; exec redispatch deduped by tool call id,
//!   which this client honors by binding every `exec_id` to one wire id)
//!   does not affect the coded surface.
//! * senpi at `c335bd57` on disk (the pinned `b8c1d3a8` is not present):
//!   `packages/ai/src/api/cursor-agent.ts` read for reconciliation only. Its
//!   transport agrees (HTTP/2 Connect `Run`, envelope framing, `turnEnded`
//!   terminal), but its dispatch executes native tools through host handlers
//!   and its retryable-stream taxonomy differs from oh-my-pi's status table.
//!   This module follows the oh-my-pi pin; the senpi comparison must be
//!   re-checked against `b8c1d3a8` at the join.
//!
//! # Provider integration
//!
//! The provider registry exposes Cursor through its HTTP-backed provider
//! path. [`decode::open_run`] owns one bidirectional Connect `Run` exchange;
//! [`lower::build_run`] prepares its opening request and [`decode::Session`]
//! handles streamed messages, tool events, and replies. [`auth`] holds the
//! sign-in steps the built-in `cursor` row's OAuth hook dispatches to: the
//! deep-control poll grant and the refresh answer.
//!
//! # Durable-assistant-entry binding
//!
//! A Cursor turn spans several dal assistant entries (one per tool-exchange
//! step), while the server persists the whole turn as one record chain. The
//! binding rule is: the turn's server records may stand for the completed
//! turn only when (a) the final step carries no tool calls, (b) the final
//! checkpoint arrived after every client write of the turn, and (c) the
//! recomputed digest over the user message, the steps' content, and the
//! paired tool results still matches. The capture then rides the final
//! step's [`StreamEvent::MessageReplay`](crate::stream::StreamEvent) event,
//! which the fold binds to that step's assistant entry; the digest and route
//! let the next request replay the records verbatim instead of re-lowering
//! history. See [`lower::CursorHistory`] and
//! [`decode::Session::capture_turn_records`]. The actor/fold-side wiring that
//! consumes the capture (session resume, checkpoint persistence across
//! steps, route-aware replay lookup) is S3-owned (`engine/dal-agent`
//! session/actor paths); this module only produces the typed capture. A turn
//! whose records cannot stand for it because replay is absent, stale, or
//! uncovered yields no capture and rebuilds from content. Present malformed
//! checkpoint records instead produce a typed refusal, preventing silent
//! rebuilding over corrupt replay state.
//!
//! # Approval seam
//!
//! Remote tool requests never touch the host directly. Model-issued MCP tool
//! calls surface as ordinary [`StreamEvent::ToolCallStarted`](crate::stream::StreamEvent)
//! / `ToolCallsDone` events and run through dal approval and execution; the
//! results return on the open stream as `McpResult` frames. Cursor-native
//! exec frames (shell, read, write, edit, grep, ls, delete, fetch, computer
//! use, subagents, hooks, and the `pi_*` CLI tools) are refused in band with
//! their typed `rejected` result or an exec `throw`, never executed.
//! Interaction queries are rejected in band with the reference's rejection
//! spellings; the reference's auto-approve path (web search, Exa, web fetch,
//! unknown queries) is deliberately not ported. See [`decode::Session`].
//!
//! # Transport note
//!
//! The codec frames the `Run` RPC: `POST <base>/agent.v1.AgentService/Run`
//! with `content-type: application/connect+proto`, one framed
//! `AgentClientMessage` carrying the run request followed by framed replies,
//! and framed `AgentServerMessage`s plus at most one end-of-stream trailer
//! frame on the way back. The reference's HTTP/1.1 `RunSSE`/`BidiAppend`
//! fallback is a different protocol and is not ported; on this transport the
//! only error terminals are the pre-stream HTTP status and the end-of-stream
//! frame. `grpc-status` trailers are an HTTP/2-only path and are not read
//! here.

use std::time::Duration;

/// The `Run` RPC path, relative to the provider base URL.
pub(crate) const RUN_PATH: &str = "/agent.v1.AgentService/Run";

/// The media type of the framed `Run` request and response bodies.
pub(crate) const CONNECT_PROTO: &str = "application/connect+proto";

/// The released Cursor agent CLI build whose protocol surface this client
/// mirrors; the service gates protocol features on it.
pub(crate) const CLIENT_VERSION: &str = "cli-2026.09.02-c22c1a3";

/// The client-type header value Cursor expects from CLI clients.
pub(crate) const CLIENT_TYPE: &str = "cli";

/// The Connect protocol version header value.
pub(crate) const PROTOCOL_VERSION: &str = "1";

/// How often the client sends a heartbeat while a run is open.
pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) mod auth;
pub(crate) mod decode;
pub(crate) mod frame;
pub(crate) mod lower;
pub(crate) mod proto;

#[cfg(test)]
mod tests;
