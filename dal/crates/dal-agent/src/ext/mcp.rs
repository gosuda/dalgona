//! MCP client runtime surface.
//!
//! The host holds at most one client per generation; `Services::mcp` routes
//! through it. When no client is registered the service fails closed with
//! `ServiceError::Denied(DenyReason::Unavailable { what: "MCP client" })`.

use super::{BoxFuture, Caller};
use crate::error::ServiceError;
use dal_core::ext::{McpRequest, McpResponse};

/// Host-configured bridge from `Services::mcp` to MCP servers.
pub trait McpClient: Send + Sync + 'static {
    /// Calls one MCP tool on behalf of `who`.
    fn call<'a>(
        &'a self,
        who: &'a Caller,
        req: McpRequest,
    ) -> BoxFuture<'a, Result<McpResponse, ServiceError>>;
}
