"""Review rules: one stream rule and one always-apply rule."""

load("@dal/v1", "dal")

plugin = dal.plugin(
    name = "review-rules",
    version = "0.1.0",
    prompt = "Review every code change for tests and error handling.",
    rules = {
        "no-todo": dal.rule(
            pattern = "TODO",
            text = "Do not leave TODO markers in new code; file a tracked task instead.",
        ),
        "always-test": dal.rule(
            text = "Run the affected package tests before reporting a change complete.",
            always_apply = True,
        ),
    },
)
