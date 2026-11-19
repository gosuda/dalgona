#### Comment removal step 9. MCP package citations

Anchor: Christopher Alexander

- Files: `internal/mcp/http.go`, `internal/mcp/tool.go`, `docs/spec/mcp.md`.
- Change: delete the comment lines of rows 50 to 52.
- Spec text, `docs/spec/mcp.md`: row 50, as the first paragraph under `## Streamable HTTP transport`: The Streamable HTTP transport MUST follow the MCP transports specification at `https://modelcontextprotocol.io/specification/2025-11-25/basic/transports`. Row 51, as a new paragraph after the paragraph that starts `An HTTP 404 response for an active session`: Closing an HTTP client with a session ID MUST send an HTTP DELETE that ends the session. That DELETE MUST finish within two seconds, and its result MUST be ignored. Row 52, as a new paragraph before the paragraph that starts `The client MUST request tools/list` under `## Tool discovery`: Tool discovery and tool calls MUST follow the MCP server tools specification at `https://modelcontextprotocol.io/specification/2025-11-25/server/tools`.
- Tests that fail before the change: none. The two-second bound is the context of `deleteSession` (`internal/mcp/http.go:236`).
- Check: `just verify` passes.
- Lands after: Comment removal step 8

