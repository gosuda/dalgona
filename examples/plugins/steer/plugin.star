"""Steer the running turn, or wake an idle turn with steering text."""

load("@dal/v1", "dal")

def steer_turn(ctx, args):
    """Hand steering text to the turn.

    Args:
        ctx: Invocation context with the turn service.
        args: Command arguments holding the steering text.

    Returns:
        A message that states whether the text steered the running
        turn or woke an idle one.
    """
    if ctx.turn.is_idle().value:
        ctx.turn.wake(text = args.text)
        return "woke an idle turn: " + args.text
    ctx.turn.steer(text = args.text)
    return "steered the running turn: " + args.text

steer_tool = dal.tool(
    description = "Add steering text to the running turn, or wake an idle turn with it.",
    input = dal.schema(text = dal.string(min_len = 1)),
    uses = ["turn.is_idle", "turn.steer", "turn.wake"],
    run = steer_turn,
)

plugin = dal.plugin(
    name = "steer",
    version = "0.1.0",
    tools = {"steer": steer_tool},
    commands = {"steer": dal.command(tool = steer_tool, positional = ["text"], description = "Steer the turn.")},
)
