load("@dal/v1", "dal")

def checkpoint(ctx, args):
    if args.action == "restore":
        return ctx.tools.exec(command = "git stash apply stash@{" + str(args.n) + "}")
    result = ctx.tools.exec(command = "git stash list")
    if not result:
        return "no checkpoints"
    return result

checkpoint_tool = dal.tool(
    description = "List or restore checkpoints.",
    input = dal.schema(
        action = dal.enum(["list", "restore"], default = "list"),
        n = dal.integer(min = 0, default = 0),
    ),
    uses = ["tools.exec"],
    run = checkpoint,
)

plugin = dal.plugin(
    name = "git-checkpoint",
    version = "0.1.0",
    commands = {
        "checkpoint": dal.command(
            tool = checkpoint_tool,
            positional = ["action", "n"],
            description = "List checkpoints.",
        ),
    },
)
