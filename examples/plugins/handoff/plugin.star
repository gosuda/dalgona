load("@dal/v1", "dal")

def handoff(ctx, args):
    note = args.note
    started = ctx.agents.start(
        prompt = "Distill this session into a handoff note for a fresh session: current goal, state, next steps, open questions. " + note,
    )
    report = ctx.agents.wait(id = started.value.id)
    text = report.value.report.text
    state = ctx.try_call(ctx.state.read, key = "handoff.md")
    if not state.ok:
        if state.error.code == "unavailable":
            return "handoff: no sidecar in an ephemeral session"
        return state.unwrap()
    saved = ctx.try_call(
        ctx.state.write,
        key = "handoff.md",
        value = text,
        expected = state.value.revision,
    )
    if not saved.ok:
        if saved.error.code == "unavailable":
            return "handoff: no sidecar in an ephemeral session"
        return saved.unwrap()
    return "Handoff written to handoff.md. Start the next session and ask for it."

handoff_tool = dal.tool(
    description = "Write a handoff note.",
    input = dal.schema(note = dal.string(default = "")),
    uses = ["agents.start", "agents.wait", "state.read", "state.write"],
    run = handoff,
)

plugin = dal.plugin(
    name = "handoff",
    version = "0.1.0",
    commands = {"handoff": dal.command(tool = handoff_tool, description = "Write a handoff note.")},
)
