load("@dal/v1", "dal")

def echo(ctx, args):
    return args

echo_tool = dal.tool(
    description = "Echo the supplied arguments back to the caller.",
    input = dal.schema(value = dal.optional(dal.string())),
    run = echo,
)

def env_probe(ctx, args):
    return {"value": ctx.env.read(args["name"])}

env_tool = dal.tool(
    description = "Read one environment variable through the env service.",
    input = dal.schema(name = dal.string()),
    uses = ["env.read"],
    run = env_probe,
)

def ask_probe(ctx, args):
    return {"answer": ctx.ask.confirm(args["text"])}

ask_tool = dal.tool(
    description = "Open a confirm request through the ask service.",
    input = dal.schema(text = dal.string()),
    uses = ["ask.confirm"],
    run = ask_probe,
)

def state_probe(ctx, args):
    prior = ctx.state.read("count")
    count = 1 if prior == None else prior + 1
    ctx.state.write("count", count)
    return {"count": count}

state_tool = dal.tool(
    description = "Bump a counter in the session state sidecar.",
    input = dal.schema(),
    uses = ["state.read", "state.write"],
    run = state_probe,
)

probe_command = dal.command(
    tool = echo_tool,
    positional = ["value"],
)

def on_before_turn(ctx, event):
    return event.append("DEVPROBE_SAW_TURN")

def on_tool_call(ctx, event):
    return event.allow()

plugin = dal.plugin(
    name = "devprobe",
    version = "0.1.0",
    tools = {
        "echo": echo_tool,
        "env": env_tool,
        "state": state_tool,
        "ask": ask_tool,
    },
    commands = {"probe": probe_command},
    hooks = [
        dal.on("before_turn", on_before_turn),
        dal.on("tool_call", on_tool_call),
    ],
)
