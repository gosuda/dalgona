load("@dal/v1", "dal")

def return_args(ctx, args):
    return args

invalid_tool = dal.tool(
    description = "Invalid tool name.",
    input = dal.schema(),
    run = return_args,
)

plugin = dal.plugin(
    name = "focus",
    version = "0.1.0",
    tools = {"BadName": invalid_tool},
)
