load("@dal/v1", "dal")

# A scripted model driven through `Host::relay`, the call the serve router
# makes. The handler uses the real scope/infer path; relay services still
# deny member sessions and never acquire a journal.
def relay_run(ctx, request):
    inner = {
        "purpose": request.purpose,
        "model": {"kind": "api", "family": "openai_chat", "model": "gpt-6-luna"},
        "system": request.system,
        "tools": request.tools,
        "context": request.context,
        "params": request.params,
        "cache_key": request.cache_key,
    }
    scope = ctx.scope(limit = 1, on_error = "settle")
    task = scope.infer(inner)
    results = scope.settle()
    result = results[0]
    if not result.ok:
        fail("relay inner inference failed: " + result.error.message)
    return result.value

relay_model = dal.model(
    id = "dalgona/relaystar",
    caps = {
        "context_window": 100000,
        "thinking": ["off"],
        "tool_use": True,
        "image_input": False,
    },
    run = relay_run,
    uses = ["models.infer"],
)

plugin = dal.plugin(
    name = "synthrelay",
    version = "0.1.0",
    inject = ["infer"],
    models = {"relaystar": relay_model},
)
