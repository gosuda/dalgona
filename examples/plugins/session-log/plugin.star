load("@dal/v1", "dal")

def remember(ctx, mark):
    seen = ctx.try_call(ctx.state.read, key = "session-log.json")
    if not seen.ok:
        if seen.error.code == "unavailable":
            return None
        seen.unwrap()
    marks = []
    if seen.value.present:
        marks = [entry for entry in seen.value.value]
    marks.append(mark)
    if len(marks) > 50:
        marks = marks[len(marks) - 50:]
    saved = ctx.try_call(
        ctx.state.write,
        key = "session-log.json",
        value = marks,
        expected = seen.value.revision,
    )
    if not saved.ok:
        if saved.error.code == "unavailable":
            return None
        saved.unwrap()
    return None

def on_start(ctx, _event):
    remember(ctx, "start")
    return None

def on_turn_end(ctx, _event):
    remember(ctx, "turn")
    return None

def show_log(ctx, _args):
    record = ctx.state.read(key = "session-log.json")
    if not record.present:
        return "session-log: no turns recorded yet"
    lines = []
    for entry in record.value:
        lines.append("- " + entry)
    return "\n".join(lines)

log_tool = dal.tool(
    description = "Show the session markers recorded by the session hooks.",
    input = dal.schema(),
    uses = ["state.read"],
    run = show_log,
)

plugin = dal.plugin(
    name = "session-log",
    version = "0.1.0",
    tools = {"log": log_tool},
    commands = {"log": dal.command(tool = log_tool, description = "Show recorded session markers.")},
    hooks = [
        dal.on("session_start", on_start, uses = ["state.read", "state.write"]),
        dal.on("turn_end", on_turn_end, uses = ["state.read", "state.write"]),
    ],
)
