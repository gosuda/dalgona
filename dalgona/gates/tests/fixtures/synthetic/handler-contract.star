load("@dal/v1", "dal")

# The one API route every inner inference targets; the scripted provider
# answers it deterministically from the gate's fixture file.
INNER = {"kind": "api", "family": "openai_chat", "model": "gpt-6-luna"}

# The wire name of this plugin's eval-only probe tool. The handler names it
# in the forwarded request's tools, so the host binds it as a private tool:
# its calls run inside the handler and never reach the session loop.
PRIVATE = "synth__probe"

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

def _forward(ctx, request, tools):
    return ctx.models.forward(
        purpose = request.purpose,
        model = INNER,
        system = request.system,
        tools = tools,
        context = request.context,
        params = request.params,
        cache_key = request.cache_key,
    )

def _has_tool_result(request):
    for item in request.context:
        if item["role"] == "tool_result":
            return True
    return False

def private_probe(ctx, args):
    return {"value": args.value}

def contract_run(ctx, request):
    # The closing round after the session loop answered the forwarded tool
    # call: one untouched forward.
    if _has_tool_result(request):
        return _forward(ctx, request, request.tools)

    # Inner results exercise the real task state, result, error, usage, and
    # cancellation operations. The fourth task is cancelled before launch,
    # so it consumes no provider step.
    scope = ctx.scope(limit = 3, on_error = "settle")
    task = scope.infer(_panel(request, INNER))
    scope.infer(_panel(request, INNER))
    scope.infer(_panel(request, INNER))
    cancelled = scope.infer(_panel(request, INNER))
    early_status = task.status()
    task_id = task.id()
    if task.error() != None:
        fail("a running task unexpectedly has an error")
    cancelled.cancel()
    cancelled_status = cancelled.status()
    cancelled_error = cancelled.error()
    results = scope.settle()
    if task.status() != "done":
        fail("the inference task did not reach done: " + task.status())
    usage = task.usage()
    settled = task.result()
    if not settled.ok:
        fail("task.result failed: " + settled.error.message)
    texts = []
    for event in settled.value.events:
        if event["type"] == "delta" and event["channel"]["type"] == "text":
            texts.append(event["text"])
    if texts != ["panel-ok"]:
        fail("unexpected task.result text: " + repr(texts))
    if usage.requests != 1 or usage.input_tokens != 12 or usage.output_tokens != 5:
        fail("task.usage was not the measured provider usage: " + repr(usage))
    if cancelled_status != "cancelled" or cancelled_error == None:
        fail("task.cancel/error did not expose owner cancellation")
    cancelled_result = cancelled.result()
    if cancelled_result.ok:
        fail("cancelled task unexpectedly returned a value")
    evidence = "task-id=" + str(task_id) + " status-before=" + early_status
    evidence += " status-after=" + task.status()
    evidence += " usage-input=" + str(usage.input_tokens)
    evidence += " usage-output=" + str(usage.output_tokens)
    evidence += " cancelled-status=" + cancelled_status
    evidence += " cancelled-error=" + cancelled_error.message

    # The member task uses the same invocation-owned scope and real mailbox
    # path. Its start result supplies the actual session id, and the list
    # check proves the id is not a synthetic handle value.
    members = ctx.scope(limit = 2, on_error = "settle")
    member = members.agent.start(prompt = "member-ready", model = "dalgona/member")
    started = member.result()
    sid = started.value.value.id
    listed = ctx.agents.list()
    listed_ok = False
    for item in listed.value:
        if item.id == sid:
            listed_ok = True
    if not listed_ok:
        fail("scope.agent id was not the live child session id")
    receipt = member.send("carry-me", "aside")
    if receipt != "delivered":
        fail("member.send did not reach the live mailbox: " + receipt)
    waited = members.agent.wait(id = sid, timeout = {"secs": 10, "nanos": 0})
    waited = waited.result()
    member_results = members.settle()
    if not waited.ok:
        fail("member wait failed: " + waited.error.message)
    member.cancel()
    after = ctx.agents.list()
    still_listed = False
    for item in after.value:
        if item.id == sid:
            still_listed = True
    if still_listed:
        fail("member.cancel left a live child session")
    evidence += " member-id=" + sid
    evidence += " member-receipt=" + receipt
    evidence += " member-status=" + member.status()
    evidence += " member-report=" + waited.value.value.report.text

    tools = []
    for tool in request.tools:
        tools.append(tool)
    tools.append({
        "name": PRIVATE,
        "description": "Answers a private probe without leaving the handler.",
        "parameters": {"type": "object", "properties": {"value": {"type": "string"}}},
    })
    forwarded = _forward(ctx, request, tools)
    return {"model": forwarded.model, "events": [_delta(evidence)] + list(forwarded.events)}

def member_run(ctx, request):
    texts = []
    for item in request.context:
        if item["role"] == "user":
            for part in item["parts"]:
                if part["type"] == "text":
                    texts.append(part["text"])
    # A Starlark model returns a core Inference: delta/usage/stop in the
    # dal_core StreamEvent dialect (flat usage, nullary stop member). The
    # provider triple (text_delta/tool_calls_done/nested usage/reason stop)
    # belongs to the scripted-provider wire only; provider_stream synthesizes
    # the empty ToolCallsDone itself, so zero tool calls emit no call event.
    return {
        "model": request.model,
        "events": [
            _delta("member-ready " + " ".join(texts)),
            {"type": "usage", "input_tokens": 12, "cached_input_tokens": 0, "output_tokens": 5, "reasoning_tokens": None, "cache_write_tokens": 0, "cost_usd": None},
            {"type": "stop", "end_turn": None},
        ],
    }

def marked_run(ctx, request):
    tools = list(request.tools)
    tools.append({
        "name": PRIVATE,
        "description": "Answers a private probe without leaving the handler.",
        "parameters": {"type": "object", "properties": {"value": {"type": "string"}}},
    })
    scope = ctx.scope(limit = 1, on_error = "settle")
    inner = {
        "purpose": request.purpose,
        "model": INNER,
        "system": request.system,
        "tools": tools,
        "context": request.context,
        "params": request.params,
        "cache_key": request.cache_key,
    }
    task = scope.infer(inner, forward = True)
    results = scope.settle()
    result = results[0]
    if not result.ok:
        fail("marked forward failed: " + result.error.message)
    return result.value

def double_run(ctx, request):
    first = _forward(ctx, request, request.tools)
    second = ctx.try_call(
        ctx.models.forward,
        purpose = request.purpose,
        model = INNER,
        system = request.system,
        tools = request.tools,
        context = request.context,
        params = request.params,
        cache_key = request.cache_key,
    )
    if second.ok:
        fail("the second forward unexpectedly succeeded")
    if second.error.message != "forward may be called at most once":
        fail("the second forward named the wrong error: " + second.error.message)
    return {
        "model": first.model,
        "events": [_delta("second-forward-refused: " + second.error.message)] + list(first.events),
    }

probe_tool = dal.tool(
    description = "Answers a private probe without leaving the handler.",
    input = dal.schema(value = dal.string()),
    visibility = "eval_only",
    run = private_probe,
)

contract_model = dal.model(
    id = "dalgona/contract",
    caps = CAPS,
    run = contract_run,
    uses = ["models.infer", "models.forward", "agents.start", "agents.wait", "agents.list"],
)

double_model = dal.model(
    id = "dalgona/double",
    caps = CAPS,
    run = double_run,
    uses = ["models.forward"],
)

member_model = dal.model(
    id = "dalgona/member",
    caps = CAPS,
    run = member_run,
    uses = [],
)

marked_model = dal.model(
    id = "dalgona/marked",
    caps = CAPS,
    run = marked_run,
    uses = ["models.infer", "models.forward"],
)

plugin = dal.plugin(
    name = "synth",
    version = "0.1.0",
    inject = ["infer", "agents"],
    tools = {"probe": probe_tool},
    models = {
        "contract": contract_model,
        "double": double_model,
        "member": member_model,
        "marked": marked_model,
    },
)
