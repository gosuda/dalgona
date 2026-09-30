load("@dal/v1", "dal")

def focus(ctx, args):
    return args

focus_tool = dal.tool(
    description = "Return the supplied focus data.",
    input = dal.schema(value = dal.optional(dal.string())),
    run = focus,
)

plugin = dal.plugin(
    name = "focus",
    version = "0.1.0",
    tools = {"focus": focus_tool},
)
