# MCP client

## Scope

The client supports MCP protocol version `2025-11-25`. It discovers server tools and calls them. It does not provide resources, prompts, sampling, or task-augmented execution.

## Lifecycle

The client MUST send `initialize` before any other request. The request MUST contain `protocolVersion`, an empty `capabilities` object, and `clientInfo` with the name `dalgona`.

The server MUST negotiate protocol version `2025-11-25`. The client MUST reject another version. The client MUST send `notifications/initialized` after a successful response.

Each JSON-RPC request MUST use a new numeric identifier. The client MUST ignore notifications and responses with other identifiers while it waits for the matching response.

The host MUST report `runtimeStatus` for each started server. A server that is still connecting MUST report `starting`. A connected server MUST report `connected`. A server whose connection or tool discovery failed MUST report `failed`, with its cause in `toolsError`. A server that needs login MUST report `authenticationRequired`. The host MUST NOT list a server that it has not started for the workspace.

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

## HTTP authentication

Authentication applies to the Streamable HTTP transport only. A server MAY set `bearer_token_env` or `oauth`, but not both. The keys are in [Configuration](config.md).

Discovery of Claude and Codex files MUST NOT read these keys. Only the native `[mcp_servers.<name>]` tables set them.

A server with an authentication source MUST send `Authorization: Bearer <token>` on every request. The session DELETE MUST carry the same header.

A redirect to another origin MUST drop the `Authorization`, `Mcp-Session-Id`, and `MCP-Protocol-Version` headers. An origin is a scheme, a host, and a port.

A bearer source MUST read its environment variable on every request. An empty variable MUST fail the request with `bearer_token_env <NAME> is not set`. That failure MUST send no HTTP request.

A bearer source has no refresh grant. An HTTP 401 response MUST fail with `MCP server <name> rejected the token in bearer_token_env <NAME>`. The client MUST NOT retry unchanged credentials.

An OAuth source MUST refresh its token once after an HTTP 401 response. The client MUST send the request again with the refreshed token. A second 401 response MUST return `MCP server <name> needs login: run dalgona mcp login <name>`. A failed refresh MUST return the same error.

Any non-2xx response other than 401 MUST NOT refresh a token. It MUST NOT select another credential or another provider.

An OAuth source MUST store its token in `auth.json` under the key `mcp-<name>`. The `account` value MUST be the canonical resource URL of the server. A token with another `account` value is no credential. The error MUST read `mcp-<name> token was issued for <account>, not <resource>; run dalgona mcp login <name>`.

The registration MUST live in `<home>/mcp-oauth.json` as one object for each server name. Each entry MUST hold `resource`, `issuer`, `authorization_endpoint`, `token_endpoint`, `client_id`, and `scopes`. The file MUST be a regular file with mode 0600. Only `dalgona mcp login` writes it.

The OAuth source MUST read the registration again on every token load and every refresh. It MUST refuse an entry for another resource before it sends a credential. It MUST refuse an entry whose `client_id` differs from a configured `oauth_client_id`. It MUST refuse an entry whose endpoints are neither https nor loopback.

The host MUST report `authStatus` for each started server. The value is `unsupported` for a stdio server and for an HTTP server without authentication. The value is `bearerToken` when the bearer variable holds a value. The value is `oAuth` when a bound token and a valid registration exist. Otherwise the value is `notLoggedIn`.

A server with `oauth` and without a bound token MUST start no network call. It MUST report `runtimeStatus` `authenticationRequired` and `toolsError` `login required: run dalgona mcp login <name>`. A server that returns 401 after a refresh MUST report the same two values. Such a server has no tools.

`dalgona mcp login <name>` MUST sign in to the `[mcp_servers.<name>]` server named `<name>`. It MUST fail when no server has that name or when the server has no `oauth` value. It MUST send one unauthenticated request to the server URL and MUST read the `WWW-Authenticate` header of a 401 response. It MUST resolve protected resource metadata, then authorization server metadata. `oauth_client_id` MUST be the client identifier when set. Otherwise the command MUST use dynamic registration. The command MUST open the authorization URL for the user and MUST accept a pasted redirect URL or code. The loopback callback MUST use `oauth_callback_port`. A successful login MUST write the registration entry and MUST store the token under `mcp-<name>` with the canonical resource as `account`. It MUST then print `logged in to MCP server <name>`.

## Tool discovery

Tool discovery and tool calls MUST follow the MCP server tools specification at `https://modelcontextprotocol.io/specification/2025-11-25/server/tools`.

The client MUST request `tools/list` until `nextCursor` is absent or empty. Each later request MUST include the returned cursor. A repeated cursor MUST fail discovery.

Each server tool MUST become a tool named `<server>__<tool>`. A name with a character outside `[A-Za-z0-9_-]`, a first character other than a letter or `_`, or more than 64 bytes MUST be rewritten. Each bad Unicode code point MUST become one `_`. A first character other than a letter or `_` MUST gain a leading `_`. A rewritten name MUST keep at most 55 leading bytes, then add `_` and the first 8 hex digits of SHA-256 of the original `<server>__<tool>`. `tools/call` MUST send the original tool name. Its class MUST be `ClassRemote`. Its `inputSchema` MUST pass through without modification. A tool definition MUST have a nonempty name and an object schema.

## Tool calls

Tool arguments MUST be a JSON object. An omitted argument value MUST become `{}`. The client MUST send `tools/call` with the original server tool name and arguments.

Text blocks MUST produce their text. Image blocks MUST produce `[image <mimeType>]`. Resource links MUST produce `[resource <name>: <uri>]`, or `[resource <uri>]` when the name is absent. Other block types MUST produce `[unsupported content <type>]`. Multiple blocks MUST be separated by one newline.

`isError: true` MUST produce a tool result with `OK = false`. It MUST NOT produce a transport error. JSON-RPC errors and malformed results MUST produce errors.

## Workspace trust

A workspace MCP server is any discovered server whose configuration file lives under the current working directory. Home and product-level servers are not workspace servers. The host MUST NOT start a workspace server process until the user trusts that workspace.

The host MUST ask once per workspace. The prompt MUST show the workspace path, every server command, its arguments, and its env keys. The host MUST send the prompt through the approval channel that asks about tool calls. Env entries reach the child process. The prompt MUST show the `PATH` and `LD_PRELOAD` keys when set. Every env value MUST appear only as `KEY=<redacted>`. The summary, reason, commands, and RPC log MUST NOT contain a raw env value. The `command` field MUST join one server line per server with newlines, not shell conjunctions. Workspace identity MUST use the canonical form defined for `mcp_trusted_workspaces` in [Configuration](config.md). The in-memory session map MUST use that form. An approval MUST persist the workspace in `mcp_trusted_workspaces` (see `config.md`) so a host restart keeps the trust. A refusal MUST hold for the session: the servers MUST stay stopped and the host MUST NOT ask again. While a workspace is untrusted, its servers MUST stay unstarted and their tools MUST stay absent. Home and product-level servers MUST start as before, without a trust prompt.

Opening a thread MUST NOT ask for workspace trust. Discovery that needs approval MUST wait for the first turn, when the actor loop runs and a client can answer the prompt.
A trust prompt that a client can answer MUST wait until the client answers or the request context ends.
When no thread is available for the approval channel, the host MUST cancel the trust request. The next turn MUST ask again.
When a trust request ends without an answer, the host MUST discard its pending approval. A later answer for that request MUST fail as an unknown approval request.

A workspace trust caller MUST refuse the workspace when the request returns an error, unless the reply is cancel.
A workspace trust request MUST decline when no client subscribes to the thread.
