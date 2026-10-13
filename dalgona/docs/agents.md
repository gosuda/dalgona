# Agents

Run subagents in the background. `agents` takes one `action`:

- `run` starts a workflow and returns at once. A workflow is a list of steps. A step without `items` is one subagent (a task). A step with `items` is a pool: one subagent per item, at most `workers` at a time (default 4, range 1 to 64). A step with `after` starts when those steps end. The other actions are `wait`, `cancel`, and `list`.
- `wait` blocks until runs or tasks end. `timeout` is seconds to wait at most, default 60, at most 600.
- `cancel` stops runs or tasks and their descendants.
- `list` shows runs, providers, and saved workflows: at most 20 runs, 200 task lines per run, then it says how to read the full report.

## Placeholders

`{{item}}` is the item of a pool. `{{step:<name>}}` is the reports of a step listed in `after`. `{{input}}` is the input of a saved workflow. There is no escape for `{{`; a different placeholder is an error. A pool prompt must contain `{{item}}`; a task must not.

## Tools and approval

Subagents see only the tools you list, by default `read` and `search`, at most 16 names. They cannot ask the user: `patch` works only in approval modes `edits` and `all`, and `exec` only in `all`. A step that lists `patch` or `exec` runs in its own git worktree by default, and its changes merge back when it ends done. Subagents cannot start subagents, hold a goal, or call `/abort`; `agents`, `create_goal`, `update_goal`, `get_goal`, and `monitor` are refused as step tools.

## Report and the one last turn

Each subagent ends by calling its own `orchestration__report` tool exactly once, with status `done`, `blocked`, or `failed` and 1 to 16,000 characters of report text. An empty report is refused, and a second report cannot replace the first. When a subagent never reports, it gets one final grace turn of 60 seconds; if it stays silent it fails with `no report after the last turn`.

## Delivery

You get one message per run, not one per subagent. Reports are held and delivered at request boundaries, only when the session is ready: after 2 seconds of quiet, 16 waiting reports, or one report waiting 60 seconds. A wake that reached the session is never repeated: only its acknowledgement is retried. Full reports stay one read away at `job://<id>`.

## Limits

1 to 32 steps per run; step names `[a-z][a-z0-9-]{0,31}`, unique; prompts 1 to 32,000 characters; pool items 1 to 1024, each 1 to 2,000 characters; `after` up to 31 steps; tools at most 16 names. Run labels at most 80 bytes. `max_runs` defaults to 16 under `[plugin.orchestration.agents]` (1 to 64); `child_max_steps` defaults to 50 (1 to 1000); `child_max_minutes` defaults to 30 (1 to 600); a session starts at most 1024 subagents (`agents_per_session` has a fixed ceiling of 1024). Isolation is `worktree` or `shared`; worktree needs git 2.17 or newer and refuses to start when git is missing or the checkout is not a repository.
