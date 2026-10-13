load("@dal/v1", "dal")

def hello(ctx, _args):
    return "Hello from a dal plugin."

hello_tool = dal.tool(
    description = "Say hello.",
    input = dal.schema(),
    run = hello,
)

plugin = dal.plugin(
    name = "hello",
    version = "0.1.0",
    tools = {"hello": hello_tool},
    prompt = "Greet the user warmly.",
    skills = {
        "hello": dal.skill(
            description = "The smallest dal plugin: one tool, one prompt section, one skill.",
            path = "skills/hello/SKILL.md",
        ),
    },
)
