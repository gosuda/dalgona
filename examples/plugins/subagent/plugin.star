load("@dal/v1", "dal")

def cancel_children(ctx, handles):
    error = ""
    for handle in handles:
        result = ctx.try_call(ctx.agents.cancel, id = handle)
        if not result.ok and not error:
            error = "subagent: child cancel failed: " + result.error.message
    return error

def start_child(ctx, task):
    result = ctx.try_call(
        ctx.agents.start,
        prompt = task,
    )
    if not result.ok:
        return None, "subagent: child start failed: " + result.error.message
    started = result.value
    if started.type != "started":
        return None, "subagent: child refused: " + started.value.reason.type
    return started.value.id, ""

def wait_child(ctx, handle):
    result = ctx.try_call(ctx.agents.wait, id = handle)
    if not result.ok:
        return None, "subagent: child wait failed: " + result.error.message
    report = result.value
    if report.type != "await":
        return None, "subagent: child did not finish: " + report.type
    return report.value.report.text, ""

def run_batch(ctx, tasks):
    handles = []
    for task in tasks:
        handle, error = start_child(ctx, task)
        if error:
            cleanup_error = cancel_children(ctx, handles)
            if cleanup_error:
                return [], cleanup_error
            return [], error
        handles.append(handle)
    answers = []
    for handle in handles:
        answer, error = wait_child(ctx, handle)
        if error:
            cleanup_error = cancel_children(ctx, handles)
            if cleanup_error:
                return [], cleanup_error
            return [], error
        answers.append(answer)
        cleanup_error = cancel_children(ctx, [handle])
        if cleanup_error:
            cancel_children(ctx, handles)
            return [], cleanup_error
    return answers, ""

def subagent(ctx, args):
    tasks = getattr(args, "tasks", dal.MISSING)
    if tasks == dal.MISSING:
        tasks = []
    if not tasks:
        single = args.task
        if single:
            tasks = [single]
    if not tasks:
        return "no task"
    answers = []
    batch = []
    for task in tasks:
        batch.append(task)
        if len(batch) == 4:
            batch_answers, error = run_batch(ctx, batch)
            if error:
                return error
            for answer in batch_answers:
                answers.append(answer)
            batch = []
    if batch:
        batch_answers, error = run_batch(ctx, batch)
        if error:
            return error
        for answer in batch_answers:
            answers.append(answer)
    return "\n".join(answers)

subagent_tool = dal.tool(
    description = "Run a task in a separate dal child session with a fresh context and return its final answer. Give one task, or up to 8 tasks that run 4 at a time. The child works in the same workspace.",
    input = dal.schema(
        task = dal.string(default = ""),
        tasks = dal.optional(dal.list(dal.string(), min_len = 1, max_len = 8)),
    ),
    uses = ["agents.start", "agents.wait", "agents.cancel"],
    run = subagent,
)

plugin = dal.plugin(
    name = "subagent",
    version = "0.1.0",
    tools = {"subagent": subagent_tool},
)
