load("@dal/v1", "dal")

def read_env(ctx, args):
    name = args.name
    if not name:
        name = ctx.config.key
    if not name:
        return dal.err("env-read", "give a name, or set key in [plugin.env-read]")
    value = ctx.env.read(key = name)
    if value == None:
        return dal.err("env-read", name + " is not set")
    return value

read_env_tool = dal.tool(
    description = "Read one environment variable. Give a name, or set a default key in [plugin.env-read].",
    input = dal.schema(name = dal.string(default = "")),
    uses = ["env.read"],
    run = read_env,
)

plugin = dal.plugin(
    name = "env-read",
    version = "0.1.0",
    config = dal.schema(key = dal.string(default = "")),
    tools = {"read": read_env_tool},
    commands = {
        "env": dal.command(
            tool = read_env_tool,
            positional = ["name"],
            description = "Read an environment variable.",
        ),
    },
)
