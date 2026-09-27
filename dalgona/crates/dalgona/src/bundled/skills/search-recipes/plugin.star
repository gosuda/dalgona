dal.plugin(name = "search-recipes", version = "0.1.0", inject = [])

dal.skill(
    name = "find-anything",
    description = "Pick the right search surface: find, grep, symbol, procs, web_search, or a deferred tool, and promote deferred tools correctly.",
    path = "find-anything/SKILL.md",
    letter2image = False,
)
