load("@dal/v1", "dal")

def mcp_settings(ctx):
    raw = getattr(ctx.config, "servers_json", dal.MISSING)
    if raw == dal.MISSING or not raw:
        return {"servers": {}, "timeout_ms": 30000}
    return json.decode(raw)

def mcp_list(ctx, args):
    servers = mcp_settings(ctx).get("servers", {})
    server = args.server
    if server:
        info = servers.get(server, {})
        if not info:
            return ""
        return server + "  " + info.get("url", "")
    lines = []
    for name in sorted(servers.keys()):
        lines.append(name + "  " + servers[name].get("url", ""))
    return "\n".join(lines)

def mcp_call(ctx, args):
    arguments = json.decode(args.arguments_json)
    response = ctx.mcp.call(server = args.server, tool = args.tool, arguments = arguments)
    return response.text

mcp_list_tool = dal.tool(
    description = "List configured MCP servers.",
    input = dal.schema(server = dal.string(default = "")),
    run = mcp_list,
)

mcp_call_tool = dal.tool(
    description = "Call one MCP tool with arguments_json containing a JSON object.",
    input = dal.schema(
        server = dal.string(default = ""),
        tool = dal.string(default = ""),
        arguments_json = dal.string(default = "{}"),
    ),
    uses = ["mcp.call"],
    run = mcp_call,
)

plugin = dal.plugin(
    name = "mcp",
    version = "0.1.0",
    config = dal.schema(servers_json = dal.optional(dal.string())),
    tools = {"mcp_list": mcp_list_tool, "mcp_call": mcp_call_tool},
)
