load("@dal/v1", "dal")

plugin = dal.plugin(
    name = "skill-pack",
    version = "0.1.0",
    skills = {
        "port-pi-extension": dal.skill(
            description = "Port a pi extension (TypeScript) to a dal plugin (Starlark). Use when the user names a pi extension, a pi package on npm, or a .ts file that calls pi.registerTool or pi on.",
            path = "skills/port-pi-extension/SKILL.md",
        ),
        "write-plugin": dal.skill(
            description = "Write a new dal plugin. Use when the user asks for a new tool, slash command, or skill.",
            path = "skills/write-plugin/SKILL.md",
        ),
    },
)
