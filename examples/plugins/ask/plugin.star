load("@dal/v1", "dal")

def questionnaire(ctx, args):
    lines = []
    for question in args.questions:
        qid = question.id
        prompt = question.prompt
        options = question.options
        if options == dal.MISSING:
            options = []
        if options:
            choices = [{"label": option} for option in options]
            choices.append({"label": "Type an answer"})
            answer = ctx.ask.select(prompt = prompt, options = choices, multi = False)
        else:
            answer = ctx.ask.text(prompt = prompt)
        if not answer:
            answer = "no answer"
        lines.append(qid + ": " + answer)
    return "\n".join(lines)

question = dal.schema(
    id = dal.string(),
    prompt = dal.string(),
    options = dal.optional(dal.list(dal.string(), min_len = 2, max_len = 10)),
    allow_other = dal.optional(dal.boolean()),
)

questionnaire_tool = dal.tool(
    description = "Ask the user several questions in a row and return every answer. Use it when you need more than one decision before you continue.",
    input = dal.schema(questions = dal.list(question, min_len = 1, max_len = 8)),
    uses = ["ask.select", "ask.text"],
    run = questionnaire,
)

plugin = dal.plugin(
    name = "ask",
    version = "0.1.0",
    tools = {"questionnaire": questionnaire_tool},
)
