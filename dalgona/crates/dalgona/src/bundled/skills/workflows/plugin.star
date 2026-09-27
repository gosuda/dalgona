dal.plugin(name = "workflows", version = "0.1.0", inject = [])

dal.skill(
    name = "initializer-and-sprints",
    description = "Split a long build into one initializer pass and one feature per session, with a progress file each session reads before it plans.",
    path = "initializer-and-sprints/SKILL.md",
    letter2image = False,
)

dal.skill(
    name = "delegate-with-contracts",
    description = "Hand work to subagents with an objective, an output format, source guidance, and boundaries; collect files and references, not prose.",
    path = "delegate-with-contracts/SKILL.md",
    letter2image = False,
)
