load("@dal/v1", "dal")

def private_fusion(ctx, args):
    return {"value": args.value}

def fusion_run(ctx, request):
    final = {"kind": "api", "family": "openai_chat", "model": "gpt-6"}
    panels = [
        {"kind": "api", "family": "anthropic", "model": "claude-opus-5"},
        {"kind": "api", "family": "openai_chat", "model": "gpt-6"},
        {"kind": "api", "family": "openai_chat", "model": "gemini-3-pro"},
    ]
    scope = ctx.scope(limit = 8, on_error = "settle", usd = 0.40)
    for panel in panels:
        scope.infer(_panel(request, panel))
    results = scope.settle()
    final_request = _panel(request, final)
    final_request["system"] = _summary(request, results)
    tools = []
    for tool in request.tools:
        tools.append(tool)
    tools.append({
        "name": "fusion__fusion",
        "description": "Use one panel result privately.",
        "parameters": {"type": "object", "properties": {"value": {"type": "string"}}},
    })
    final_request["tools"] = tools
    return ctx.models.forward(final_request)

def _has_result(request):
    for item in request.context:
        if item["role"] == "tool_result" and item["name"] == "read":
            return True
    return False

def _summary(request, results):
    panels = []
    for result in results:
        if result.ok:
            for event in result.value.events:
                if event["type"] == "delta" and event["channel"]["type"] == "text":
                    panels.append(event["text"])
    expected = _expected_panels(request)
    if panels != expected:
        fail("panels [" + ", ".join(panels) + "] do not match [" + ", ".join(expected) + "]")
    return "panels: " + ", ".join(panels)

def _expected_panels(request):
    if _has_result(request):
        return [
            "Opus panel after tool result",
            "GPT panel after tool result",
            "Gemini panel after tool result",
        ]
    return ["Opus panel", "GPT panel", "Gemini panel"]

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

fusion_tool = dal.tool(
    description = "Use one panel result privately.",
    input = dal.schema(value = dal.optional(dal.string())),
    visibility = "eval_only",
    run = private_fusion,
)

fusion_model = dal.model(
    id = "dalgona/fusion",
    caps = {
        "context_window": 100000,
        "thinking": ["off"],
        "tool_use": True,
        "image_input": False,
    },
    run = fusion_run,
    uses = ["models.infer", "models.forward"],
)

plugin = dal.plugin(
    name = "fusion",
    version = "0.1.0",
    inject = ["infer"],
    tools = {"fusion": fusion_tool},
    models = {"fusion": fusion_model},
)
