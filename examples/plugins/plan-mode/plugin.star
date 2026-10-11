load("@dal/v1", "dal")

# V1 before_turn hooks cannot read state; the previous dynamic guidance was:
# Plan mode is on. Explore and write the plan; change nothing.

def plan(ctx, _args):
    record = ctx.state.read(key = "plan-mode")
    if record.present and record.value == "on":
        ctx.state.write(key = "plan-mode", value = "off", expected = record.revision)
        return "plan mode: off"
    ctx.state.write(key = "plan-mode", value = "on", expected = record.revision)
    return "plan mode: on"

plan_tool = dal.tool(
    description = "Toggle plan mode.",
    input = dal.schema(),
    uses = ["state.read", "state.write"],
    run = plan,
)

plugin = dal.plugin(
    name = "plan-mode",
    version = "0.1.0",
    commands = {"plan": dal.command(tool = plan_tool)},
)
