load("@dal/v1", "dal")

def load_patterns(ctx):
    patterns = ["rm -rf", "sudo", "force"]
    extra = getattr(ctx.config, "patterns", dal.MISSING)
    if extra != dal.MISSING:
        patterns = patterns + [pattern for pattern in extra]
    return patterns

def on_tool_call(ctx, event):
    tool = str(event.tool)
    summary = str(event.args)
    for pattern in load_patterns(ctx):
        if pattern and pattern in summary:
            return event.block(reason = 'permission-gate: blocked ' + tool + ' ' + summary + ' (matches "' + pattern + '")')
    return event.allow()

def gate(ctx, _args):
    return "permission-gate: " + str(len(load_patterns(ctx))) + " patterns active"

gate_tool = dal.tool(
    description = "Show active patterns.",
    input = dal.schema(),
    run = gate,
)

plugin = dal.plugin(
    name = "permission-gate",
    version = "0.1.0",
    config = dal.schema(patterns = dal.optional(dal.list(dal.string()))),
    commands = {"gate": dal.command(tool = gate_tool, description = "Show active patterns.")},
    hooks = [dal.on("tool_call", on_tool_call)],
)
