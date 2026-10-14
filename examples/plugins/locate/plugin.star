load("@dal/v1", "dal")

def lookup(ctx, args):
    if args.path:
        view = ctx.tools.read(args.path)
        text = "\n".join([row.text for row in view.rows])
        return dal.output(value = text, view = view)
    if not args.pattern:
        return dal.err("locate", "give a pattern to search for, or a path to read")
    page = ctx.tools.search(args.pattern)
    matches = [{"path": hit.path, "line": hit.line, "text": hit.text} for hit in page.matches]
    return {"matches": matches, "truncated": page.truncated}

lookup_tool = dal.tool(
    description = "Find text in the workspace or read one file. Give a pattern to search for, or a path to read.",
    input = dal.schema(
        pattern = dal.string(default = ""),
        path = dal.string(default = ""),
    ),
    uses = ["tools.read", "tools.search"],
    run = lookup,
)

plugin = dal.plugin(
    name = "locate",
    version = "0.1.0",
    tools = {"lookup": lookup_tool},
    commands = {
        "locate": dal.command(
            tool = lookup_tool,
            positional = ["pattern"],
            description = "Search the workspace or read a file.",
        ),
    },
)
