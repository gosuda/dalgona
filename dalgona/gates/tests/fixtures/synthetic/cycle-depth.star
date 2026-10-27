load("@dal/v1", "dal")

# Typed synthetic routing failures through the real product: a scope child
# that re-infers its own id is a `SyntheticCycle`, and a scope chain that
# reaches a fifth id is a `SyntheticDepth`. Each handler surfaces the exact
# typed message as its turn text, so the gate asserts the chain contents.
CAPS = {
    "context_window": 100000,
    "thinking": ["off"],
    "tool_use": True,
    "image_input": False,
}

def _panel(request, model):
    return {
        "purpose": request.purpose,
        "model": model,
        "system": request.system,
        "tools": request.tools,
        "context": request.context,
        "params": request.params,
        "cache_key": request.cache_key,
    }

def _delta(text):
    return {"type": "delta", "channel": {"type": "text"}, "text": text}

def _usage():
    return {
        "type": "usage",
        "input_tokens": 1,
        "cached_input_tokens": 0,
        "output_tokens": 1,
        "reasoning_tokens": None,
        "cache_write_tokens": 0,
        "cost_usd": None,
    }

def _record(identity, texts):
    events = []
    for text in texts:
        events.append(_delta(text))
    events.append(_usage())
    events.append({"type": "stop", "end_turn": None})
    return {"model": {"kind": "synthetic", "id": identity}, "events": events}

def _child_texts(result):
    if result.ok:
        texts = []
        for event in result.value.events:
            if event["type"] == "delta" and event["channel"]["type"] == "text":
                texts.append(event["text"])
        return texts
    return [result.error.message]

def _run_depth(ctx, request, child, identity):
    scope = ctx.scope(limit = 1, on_error = "settle")
    scope.infer(_panel(request, {"kind": "synthetic", "id": child}))
    results = scope.settle()
    texts = ["ran " + identity] + _child_texts(results[0])
    return _record(identity, texts)

def run_cycle(ctx, request):
    scope = ctx.scope(limit = 1, on_error = "settle")
    scope.infer(_panel(request, {"kind": "synthetic", "id": "dalgona/cycle"}))
    results = scope.settle()
    if results[0].ok:
        fail("inferring this model from itself must not run")
    return _record("dalgona/cycle", [results[0].error.message])

def run_dd1(ctx, request):
    return _run_depth(ctx, request, "dalgona/dd2", "dalgona/dd1")

def run_dd2(ctx, request):
    return _run_depth(ctx, request, "dalgona/dd3", "dalgona/dd2")

def run_dd3(ctx, request):
    return _run_depth(ctx, request, "dalgona/dd4", "dalgona/dd3")

def run_dd4(ctx, request):
    return _run_depth(ctx, request, "dalgona/dd5", "dalgona/dd4")

def run_dd5(ctx, request):
    # Registered so the fifth chain entry resolves; the depth refusal fires
    # before this handler ever runs.
    return _record("dalgona/dd5", ["ran dalgona/dd5"])

cycle_model = dal.model(
    id = "dalgona/cycle",
    caps = CAPS,
    run = run_cycle,
    uses = ["models.infer"],
)

dd1_model = dal.model(id = "dalgona/dd1", caps = CAPS, run = run_dd1, uses = ["models.infer"])
dd2_model = dal.model(id = "dalgona/dd2", caps = CAPS, run = run_dd2, uses = ["models.infer"])
dd3_model = dal.model(id = "dalgona/dd3", caps = CAPS, run = run_dd3, uses = ["models.infer"])
dd4_model = dal.model(id = "dalgona/dd4", caps = CAPS, run = run_dd4, uses = ["models.infer"])
dd5_model = dal.model(id = "dalgona/dd5", caps = CAPS, run = run_dd5, uses = ["models.infer"])

plugin = dal.plugin(
    name = "synthcyc",
    version = "0.1.0",
    inject = ["infer"],
    models = {
        "cycle": cycle_model,
        "dd1": dd1_model,
        "dd2": dd2_model,
        "dd3": dd3_model,
        "dd4": dd4_model,
        "dd5": dd5_model,
    },
)
