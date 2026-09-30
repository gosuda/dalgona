load("@dal/v1", "dal")

def subagent(ctx, args):
    tasks = args.tasks
    if tasks == dal.MISSING:
        tasks = []
    if not tasks:
        single = args.task
        if single:
            tasks = [single]
    if not tasks:
        return "no task"
    handles = []
    for task in tasks:
        started = ctx.agents.start(prompt = task)
        handles.append(started.value.id)
        if len(handles) >= 8:
            break
    answers = []
    active = handles[0:4]
    rest = handles[4:]
    for handle in active:
        report = ctx.agents.wait(id = handle)
        answers.append(report.value.report.text)
    for handle in rest:
        report = ctx.agents.wait(id = handle)
        answers.append(report.value.report.text)
    return "\n".join(answers)

subagent_tool = dal.tool(
    description = "Run a task in a separate dal child session with a fresh context and return its final answer. Give one task, or up to 8 tasks that run 4 at a time. The child works in the same workspace.",
    input = dal.schema(
        task = dal.string(default = ""),
        tasks = dal.optional(dal.list(dal.string(), min_len = 1, max_len = 8)),
    ),
    uses = ["agents.start", "agents.wait"],
    run = subagent,
)

plugin = dal.plugin(
    name = "subagent",
    version = "0.1.0",
    tools = {"subagent": subagent_tool},
)
