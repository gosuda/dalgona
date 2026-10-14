"""Fetch one URL over HTTP and report its status and body size."""

load("@dal/v1", "dal")

def fetch_url(ctx, args):
    """Fetch one URL and return its status line.

    Args:
        ctx: Invocation context with the net service.
        args: Command arguments holding the url to fetch.

    Returns:
        One line with the status and body size, or a fetch_failed
        domain error when the request or the status fails.
    """
    result = ctx.try_call(ctx.net.fetch, method = "GET", url = args.url)
    if not result.ok:
        return dal.err("fetch_failed", "Could not fetch " + args.url + ": " + result.error.message)
    response = result.value
    if response.status != 200:
        return dal.err("fetch_failed", "GET " + args.url + " returned status " + str(response.status))
    return "GET " + args.url + ": status 200, " + str(len(response.body)) + " body bytes"

fetch_tool = dal.tool(
    description = "Fetch one URL with an HTTP GET and report its status and body size.",
    input = dal.schema(url = dal.string()),
    uses = ["net.fetch"],
    run = fetch_url,
)

plugin = dal.plugin(
    name = "fetch",
    version = "0.1.0",
    tools = {"fetch": fetch_tool},
    commands = {"fetch": dal.command(tool = fetch_tool, positional = ["url"], description = "Fetch one URL.")},
)
