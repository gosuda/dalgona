# mcp: List and call MCP tools.

## Ported from
the shape of pi-mcp-adapter

## What it shows
Plugin settings and one operation. `mcp_list` prints the servers named in the `servers_json` setting, one `name  url` line each. `mcp_call` takes a `server`, a `tool`, and `arguments_json` holding a JSON object, calls the tool with `mcp.call`, and returns the response text. Only `mcp_call` declares an operation.

## What differs from pi
This plugin starts no MCP server and holds no connection. `mcp_list` reads only the setting. `mcp_call` reaches whatever MCP client the host has; dalgona ships one, and dal does not, so there the call is unavailable. stdio servers need dalgona's `mcp` battery or an external bridge program.

## Install
Copy to the dal plugins data root and add `mcp` to `plugins`.

## Settings
`servers_json` is optional text holding a JSON object with a `servers` map:

```toml
[plugin.mcp]
servers_json = '{"servers": {"docs": {"url": "https://example.com/mcp"}}}'
```
