"""Toggle a persisted plan-mode setting with the /plan command."""

load("@dal/v1", "dal")

def plan(ctx, _args):
    """Toggle plan mode and return its new state.

    Args:
        ctx: Invocation context with access to plugin state.
        _args: Unused command arguments.

    Returns:
        A message that states whether plan mode is on or off.
    """
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
