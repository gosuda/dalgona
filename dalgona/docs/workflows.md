# Workflows

A saved workflow is one table under `[plugin.orchestration.workflows.<name>]` in config. It holds an optional `description` (at most 200 characters) and `steps` — the same steps an inline `agents run` takes.

## Shape

```toml
[plugin.orchestration.workflows.audit-routes]
description = "Audit route handlers for missing auth checks."

[[plugin.orchestration.workflows.audit-routes.steps]]
name = "find"
prompt = "List every HTTP route handler file under src/. Put one path per line in your report and nothing else."

[[plugin.orchestration.workflows.audit-routes.steps]]
name = "audit"
after = ["find"]
items_from = "find"
workers = 8
prompt = "Check {{item}} for route handlers that skip the auth check. {{input}}"
```

`items_from` names one earlier task step in `after`: the step becomes a pool over the non-empty lines of that step's report, each cut to 2,000 characters, at most 1,024 lines. `{{input}}` needs input: run the saved workflow with `input`, or the run is refused.

## Refusals

An invalid workflow is refused with the exact reason: unknown or missing fields, a name that does not match `[a-z][a-z0-9-]{0,31}` or is used twice, a prompt outside 1 to 32,000 characters, `items` and `items_from` together, `items` outside 1 to 1024 items of 1 to 2,000 characters, `workers` on a task or outside 1 to 64, `after` naming a missing step or forming a cycle, `items_from` not in `after` or naming a pool, a pool prompt without `{{item}}`, `{{item}}` in a task, `{{step:<name>}}` without the name in `after`, `{{input}}` without input, an unknown `{{...}}` placeholder, or a tool list over 16 names. A step tool naming `agents`, `report`, `create_goal`, `update_goal`, `get_goal`, or `monitor` is refused: `agents: step <name>: tool <tool> is not for subagents.`

A saved workflow that does not decode is not a startup error. Running it says `agents: saved workflow "<name>" is invalid: <reason>. Fix [plugin.orchestration.workflows.<name>] in config.` An unknown name says `agents: no saved workflow "<name>".` and lists the known names. A saved workflow with `{{input}}` and no input is refused until input is given.
