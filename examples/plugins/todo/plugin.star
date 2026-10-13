load("@dal/v1", "dal")

def todo_load(ctx):
    record = ctx.state.read(key = "todos")
    items = []
    if record.present:
        for item in record.value:
            items.append({"id": item.id, "text": item.text, "done": item.done})
    return items, record.revision

def todo_save(ctx, items, revision):
    ctx.state.write(key = "todos", value = items, expected = revision)

def todo(ctx, args):
    action = args.action
    if action == "list":
        items, _revision = todo_load(ctx)
        if not items:
            return "No todos"
        lines = []
        for item in items:
            mark = "x" if item["done"] else " "
            lines.append("[" + mark + "] #" + str(item["id"]) + ": " + item["text"])
        return "\n".join(lines)
    if action == "add":
        text = args.text
        if not text:
            return "todo: text is required for add"
        items, revision = todo_load(ctx)
        new_id = 1
        for item in items:
            if item["id"] >= new_id:
                new_id = item["id"] + 1
        items.append({"id": new_id, "text": text, "done": False})
        todo_save(ctx, items, revision)
        return "Added todo #" + str(new_id) + ": " + text
    if action == "toggle":
        target = args.id
        if not target:
            return "todo: id is required for toggle"
        items, revision = todo_load(ctx)
        for item in items:
            if item["id"] == target:
                item["done"] = not item["done"]
                todo_save(ctx, items, revision)
                if item["done"]:
                    return "Todo #" + str(target) + " completed"
                return "Todo #" + str(target) + " uncompleted"
        return "Todo #" + str(target) + " uncompleted"
    if action == "clear":
        items, revision = todo_load(ctx)
        count = len(items)
        todo_save(ctx, [], revision)
        return "Cleared " + str(count) + " todos"
    return "No todos"

todo_tool = dal.tool(
    description = "Keep a todo list for this task. Actions: list, add (text), toggle (id), clear.",
    input = dal.schema(
        action = dal.enum(["list", "add", "toggle", "clear"], default = "list"),
        text = dal.string(default = ""),
        id = dal.integer(default = 0),
    ),
    uses = ["state.read", "state.write"],
    run = todo,
)


plugin = dal.plugin(
    name = "todo",
    version = "0.1.0",
    tools = {"todo": todo_tool},
    commands = {"todos": dal.command(tool = todo_tool, description = "Print the todo list.")},
)
