# MCP client

## Scope

The client supports MCP protocol version `2025-11-25`. It discovers server tools and calls them. It does not provide resources, prompts, sampling, or task-augmented execution.

## Lifecycle

The client MUST send `initialize` before any other request. The request MUST contain `protocolVersion`, an empty `capabilities` object, and `clientInfo` with the name `dalgona`.

The server MUST negotiate protocol version `2025-11-25`. The client MUST reject another version. The client MUST send `notifications/initialized` after a successful response.

Each JSON-RPC request MUST use a new numeric identifier. The client MUST ignore notifications and responses with other identifiers while it waits for the matching response.

The host MUST report `runtimeStatus` for each started server. A server that is still connecting MUST report `starting`. A connected server MUST report `connected`. A server whose connection or tool discovery failed MUST report `failed`, with its cause in `toolsError`. The host MUST NOT list a server that it has not started for the workspace.

## stdio transport

The client MUST start the configured command in its own process group (a job object on Windows) with its configured arguments. The child environment MUST contain the caller environment followed by the configured entries.

Each message MUST be one UTF-8 JSON object followed by a newline. Messages MUST NOT contain embedded newlines. The client MUST keep the last 4096 bytes of child standard error output. The client MUST include that tail in each stdio transport error. Tool output passes the secret mask before it enters the journal.

A stdio request MUST finish within five minutes unless its caller sets an earlier deadline. Cancellation MUST close the whole child process group and return without waiting for the pending pipe operation.

A process crash MUST invalidate the transport. The client MUST restart the process, repeat initialization, and retry the interrupted request once. The client MUST return the second failure without another retry.

Closing the client MUST close standard input, terminate the whole process group, and reap the direct child. Terminating a child that already exited MUST succeed. The transport MUST bound pipe waits with WaitDelay so a grandchild holding stdout cannot hang Close.

## Streamable HTTP transport

The Streamable HTTP transport MUST follow the MCP transports specification at `https://modelcontextprotocol.io/specification/2025-11-25/basic/transports`.

Every message MUST use an HTTP POST to the configured URL. The client MUST send `Content-Type: application/json` and `Accept: application/json, text/event-stream`.

After initialization, every request MUST contain `MCP-Protocol-Version: 2025-11-25`. When the server supplies `Mcp-Session-Id`, every later request MUST contain the same value. The session identifier MUST contain only visible ASCII bytes.

The client MUST accept either `application/json` or `text/event-stream` responses. JSON responses MUST contain exactly one matching JSON-RPC response. SSE responses MAY contain other messages before the matching response. Adjacent SSE `data` lines MUST form one event payload. The client MUST ignore empty events and comments.

A JSON response MUST NOT exceed 16 MiB. One SSE line MUST NOT exceed 16 MiB. An HTTP exchange MUST finish within five minutes unless its caller sets an earlier deadline.

An HTTP 404 response for an active session MUST start a replacement session and retry the request once. Other non-2xx responses MUST fail the request.

Closing the HTTP client MUST send an HTTP DELETE with `Mcp-Session-Id` when a session exists. The client MUST wait at most two seconds for that DELETE and MUST ignore its failure.

The error for a non-2xx response MUST contain at most the first 8192 bytes of the response body.

## Tool discovery

Tool discovery and tool calls MUST follow the MCP server tools specification at `https://modelcontextprotocol.io/specification/2025-11-25/server/tools`.

The client MUST request `tools/list` until `nextCursor` is absent or empty. Each later request MUST include the returned cursor. A repeated cursor MUST fail discovery.

Each server tool MUST become a tool named `<server>__<tool>`. A name with a character outside `[A-Za-z0-9_-]`, a first character other than a letter or `_`, or more than 64 bytes MUST be rewritten. Each bad character MUST become `_`. A first character other than a letter or `_` MUST gain a leading `_`. A rewritten name MUST keep at most 55 leading bytes, then add `_` and the first 8 hex digits of SHA-256 of the original `<server>__<tool>`. `tools/call` MUST send the original tool name. Its class MUST be `ClassRemote`. Its `inputSchema` MUST pass through without modification. A tool definition MUST have a nonempty name and an object schema.

## Tool calls

Tool arguments MUST be a JSON object. An omitted argument value MUST become `{}`. The client MUST send `tools/call` with the original server tool name and arguments.

Text blocks MUST produce their text. Image blocks MUST produce `[image <mimeType>]`. Resource links MUST produce `[resource <name>: <uri>]`, or `[resource <uri>]` when the name is absent. Other block types MUST produce `[unsupported content <type>]`. Multiple blocks MUST be separated by one newline.

`isError: true` MUST produce a tool result with `OK = false`. It MUST NOT produce a transport error. JSON-RPC errors and malformed results MUST produce errors.

## Workspace trust

A workspace MCP server is any discovered server whose configuration file lives under the current working directory. Home and product-level servers are not workspace servers. The host MUST NOT start a workspace server process until the user trusts that workspace.

The host MUST ask once per workspace. The prompt MUST show the workspace path, every server command, its arguments, and its env keys. The host MUST send the prompt through the approval channel that asks about tool calls. Env entries reach the child process. The prompt MUST show the `PATH` and `LD_PRELOAD` keys when set. Every env value MUST appear only as `KEY=<redacted>`. The summary, reason, commands, and RPC log MUST NOT contain a raw env value. The `command` field MUST join one server line per server with newlines, not shell conjunctions. The workspace identity is the canonical path (`filepath.Abs` plus `EvalSymlinks`, falling back to `Clean`): the in-memory session map and `mcp_trusted_workspaces` MUST use this same form. An approval MUST persist the workspace in `mcp_trusted_workspaces` (see `config.md`) so a host restart keeps the trust. A refusal MUST hold for the session: the servers MUST stay stopped and the host MUST NOT ask again. While a workspace is untrusted, its servers MUST stay unstarted and their tools MUST stay absent. Home and product-level servers MUST start as before, without a trust prompt.

Opening a thread MUST NOT ask for workspace trust. Discovery that needs approval MUST wait for the first turn, when the actor loop runs and a client can answer the prompt.
A trust prompt that a client can answer MUST wait until the client answers or the request context ends.
When no thread is available for the approval channel, the host MUST cancel the trust request. The next turn MUST ask again.
When a trust request ends without an answer, the host MUST discard its pending approval. A later answer for that request MUST fail as an unknown approval request.

A workspace trust caller MUST refuse the workspace when the request returns an error, unless the reply is cancel.
A workspace trust request MUST decline when no client subscribes to the thread.
