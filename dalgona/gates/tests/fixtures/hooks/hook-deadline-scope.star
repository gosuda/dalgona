load("@dal/v1", "dal")

# Hook-deadline gate fixture: one scripted model whose run opens a
# three-member scope and then blocks forever collecting it. The gate's Rust
# before_turn hook drives a child inference on this model, so the scope
# members stay unfinished past the 5s hook deadline. The member route names
# the hanging-member model the same gate registers on its Rust extension.

def hold(ctx, request):
    scope = ctx.scope(limit = 3, on_error = "settle")
    req = {
        "purpose": request.purpose,
        "model": {"kind": "synthetic", "id": "hook-gate/member"},
        "system": request.system,
        "tools": request.tools,
        "context": request.context,
        "params": request.params,
        "cache_key": request.cache_key,
    }
    scope.infer(req)
    scope.infer(req)
    scope.infer(req)
    scope.settle()

hold_model = dal.model(
    id = "hook-deadline-scope/hold",
    caps = {
        "context_window": 100000,
        "thinking": ["off"],
        "tool_use": False,
        "image_input": False,
    },
    run = hold,
    uses = ["models.infer"],
)

plugin = dal.plugin(
    name = "hook-deadline-scope",
    version = "0.1.0",
    inject = ["infer"],
    models = {"hold": hold_model},
)
