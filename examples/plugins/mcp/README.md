# mcp: List and call MCP tools.

## Ported from
the shape of pi-mcp-adapter

## What it shows
Plugin settings and the `mcp.call` operation. Both tools declare `uses = ["mcp.call"]`:

- `mcp_list`: prints the servers named in the `servers_json` setting, formatted as `name  url` lines.
- `mcp_call`: takes `server`, `tool`, and `arguments_json` (holding a JSON object string, default `{}`). It decodes the arguments, calls the remote tool with `mcp.call`, and returns the response text.

If an MCP response reports an error (`isError = true`), both tools return a failed tool outcome under the `mcp_error` error code with the remote message.

## What differs from pi
This plugin starts no MCP server and holds no direct connection. `mcp_list` reads configured servers from the setting. Pure dal does not bundle an MCP client backend, so calls fail closed as unavailable unless a client implementation is composed into the host; dalgona provides an MCP battery. stdio servers require an external bridge or battery.

## Install
Copy to the dal plugins data root and add `mcp` to `plugins`.

## Settings
`servers_json` is optional text holding a JSON object with a `servers` map:

```toml
[plugin.mcp]
servers_json = '{"servers": {"docs": {"url": "https://example.com/mcp"}}}'
```
