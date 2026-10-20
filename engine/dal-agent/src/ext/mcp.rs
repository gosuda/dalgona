//! MCP client runtime surface.
//!
//! The host holds at most one client per generation; `Services::mcp` routes
//! through it. When no client is registered the service fails closed with
//! `ServiceError::Denied(DenyReason::Unavailable { what: "MCP client" })`.

use super::{BoxFuture, Caller};
use crate::error::ServiceError;
use dal_core::ext::{McpRequest, McpResponse};
use tokio_util::sync::CancellationToken;

/// Host-configured bridge from `Services::mcp` to MCP servers.
pub trait McpClient: Send + Sync + 'static {
    /// Calls one MCP tool on behalf of `who`.
    ///
    /// `cancel` is the call's own cancellation: a child of the caller's turn
    /// token while that turn is live, the session token otherwise. A client
    /// waiting on a server reply must stop waiting when it fires, tell the
    /// server the request is abandoned, and return
    /// [`ServiceError::Cancelled`]. The turn driver cannot end a cancelled
    /// turn until every in-flight call returns, so a client that ignores
    /// `cancel` holds the turn open until its own call deadline.
    fn call<'a>(
        &'a self,
        who: &'a Caller,
        req: McpRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<McpResponse, ServiceError>>;
}
