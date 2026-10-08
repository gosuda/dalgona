//! Transport adapters for dal's public host contract.
//!
//! This crate keeps protocol framing and transport conversion at the edge. It reuses
//! `dal-core` values and the public host operations from `dal-agent`.

#![forbid(unsafe_code)]
/// A2A task states, transitions, and stable error mapping.
pub mod a2a;
/// Agent Client Protocol branches, versions 1 and 2.
pub mod acp;
/// Codex app-server adapter over stdio JSONL and WebSocket.
pub mod codex;
/// Wire error and server lifecycle error types.
pub mod error;
/// JSON-RPC envelope decoding and encoding.
pub mod jsonrpc;
/// Protocol version, capability negotiation, schema, and prompt helpers.
pub mod protocol;
/// Remote host and agent adapters over local sockets and WebSocket.
pub mod remote;
/// OpenAI-compatible route matching, canonical digests, and router options.
pub mod router;
/// Version-1 dal RPC connection state and method dispatch.
pub mod rpc;
/// HTTP/1.1 serve listener with router, A2A, and WebSocket upgrades.
pub mod serve;
/// Serve token creation, loading, and digest comparison.
pub mod token;
/// Line-framed local, standard-stream, and in-memory transports.
pub mod transport;
pub use acp::{AcpVersion, serve_acp};
pub use codex::serve_codex;
pub use error::{ServeError, WireError};
pub use jsonrpc::{ErrorObject, Id, JsonRpcError, Message, decode_jsonrpc, encode_jsonrpc};
pub use protocol::{
    CAPABILITIES, PROTOCOL_VERSION, acp_prompt_error, acp_prompt_result, protocol_schema,
};
pub use remote::{RemoteAgent, RemoteDelivery, RemoteEndpoint, RemoteHost, RemoteSubscription};
pub use router::serve::serve_router;
pub use rpc::{serve_rpc, serve_rpc_draining};
pub use serve::ServeHandle;
pub use transport::{
    ConnectionFuture, FrameWriter, LocalSocketError, LocalTransport, MAX_FRAME_BYTES, MemoryPeer,
    MemoryTransport, ReadFrameError, StdioTransport, Transport, WebSocketTransport,
    default_rpc_path, serve_local, serve_websocket,
};

#[cfg(test)]
mod tests;
