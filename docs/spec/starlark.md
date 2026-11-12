# Durable Starlark

This specification defines package `internal/script`: the run engine, the `eval` tool, tool modes, workflows, and plugins. The standard library is defined by `internal/script/stdlib`.

## Dialect

A script MUST parse with `syntax.FileOptions{Set: true, While: true, TopLevelControl: true, GlobalReassign: true, Recursion: true}`. A run MUST stop with status `limit_exceeded` after 1,000,000,000 execution steps. A done context MUST cancel the Starlark thread.

When the last top-level statement is an expression, its value MUST be the run result. Otherwise the result MUST be `None`. The result MUST encode as JSON. A result that does not encode MUST fail with code `result` and stage `result`.

`print` MUST append each line to the run prints. Prints MUST stop at 1000 lines or 64 KiB. After the limit, `truncated` MUST be `true`.

## The Effects seam

The agent host implements one interface. It is built from small interfaces that each name one behavior.

```go
type Journal interface {
	Started(start RunStart) (RunStart, *Outcome, error)
	Lookup(run string, n int) (Recorded, bool)
	Record(slot Slot, result jsontext.Value) error
	Completed(run string, outcome Outcome) error
}
type ToolCaller interface {
	CallTool(ctx context.Context, slot Slot, name string, args jsontext.Value) (jsontext.Value, error)
}
type Children interface {
	Spawn(ctx context.Context, slot Slot, prompt string, opts SpawnOpts) (string, error)
	Wait(ctx context.Context, ids []string) ([]string, error)
}
type Judger interface {
	Judge(ctx context.Context, slot Slot, req JudgeRequest) (jsontext.Value, error)
}
type Logger interface {
	Log(run, msg string)
}
type Effects interface {
	Journal
	ToolCaller
	Children
	Judger
	Logger
}
```

- `Started` MUST journal `script.started` when the run id is new in the open turn and return the given start. For a known open run id it MUST NOT journal and MUST return the pinned start. A known run id that completed earlier in the same open turn MUST return its recorded outcome. That outcome applies only when the request has the same entry point, source, and arguments. The engine MUST NOT run the source again. A different request with that run id MUST fail as a duplicate run id. A run id from a prior turn MUST start fresh. The recorded outcome has the status that the run ended with. A completed run has the value. A failed or stopped run has the code, the message, and the stage. It has no prints. The engine MUST execute the pinned `Source` with the pinned `Seed`.
- `Lookup` MUST return the recorded effect `n` of a run in the open turn. `Recorded.Interrupted` MUST be `true` when the effect started and its outcome is unknown after the host applied the recovery table. `Recorded.Error` MUST hold the failure text of a failed effect. `Recorded.Denied` MUST be `true` only for a recorded denial.
- `Record` MUST journal `script.effect` with `run_id`, `n`, `op`, `args_sha`, and `result`. The engine calls `Record` for `time`, `sleep`, and `judge`.
- `Completed` MUST journal `script.completed` for the run: `ok` and `value` for status `completed`, otherwise `ok=false` and the error message. The engine MUST call it once when any started run ends: an eval cell, a plugin tool, hook, or command, and a workflow.
- `CallTool` MUST run the tool through approvals and the thread's runner. It MUST journal `tool.started` and `tool.completed` with `call_id = "<run>/<n>"`. A later `Lookup` of that index MUST return the recorded value. A tool failure MUST return an error. A denial MUST wrap `script.ErrDenied`.
- `Spawn` MUST journal `child.spawned` with `call_id = "<run>/<n>"`. A later `Lookup` MUST return the subagent id as a JSON string.
- `Wait` MUST return the final text of each subagent, in input order.
- `Judge` MUST ask the decider for `req.Model`, or for the resolved `[approval_decider] decider` when the model is empty. It MUST return an object that maps each question name to an object with `choice`, `noul`, `score`, `probabilities`, and `confidence` as the answer has them.

## Effects and replay

Each effect MUST get an index `n` when the script submits it. The first effect of a run is `1`. Concurrent effects MUST keep their submission index, so they replay in the same slots.

| Builtin | `op` | Arguments hashed |
| --- | --- | --- |
| `ctx.tools.<name>(**kw)` | `tool:<name>` | the keyword arguments as one JSON object |
| `ctx.spawn` | `spawn` | `{"prompt","runner","model"}` |
| `time.time()` | `time` | `null` |
| `ctx.sleep(s)`, `time.sleep(s)` | `sleep` | the duration in nanoseconds |
| `ctx.judge`, each `ctx.judge_batch` item | `judge` | `{"state","questions","model"}` |

`args_sha` MUST be the lowercase hex SHA-256 of the argument bytes (`script.ArgsSHA`). `ctx.wait` and `ctx.log` MUST NOT take an index.

For each effect the engine MUST call `Lookup` first:

- No record: the effect MUST run live.
- A record with a different `op` or `args_sha`: the run MUST stop with status `failed`, code `nondeterminism`, and message `nondeterminism at effect <n>: recorded <op>, got <op>`.
- `Interrupted`: the run MUST stop with status `indeterminate`.
- `Denied`: the run MUST stop with status `denied`. Any other `Error`: the effect MUST fail with that text.
- Otherwise the effect MUST return the recorded result and MUST NOT run again.

`sleep` MUST record the absolute wake deadline in Unix nanoseconds before it waits. A replayed sleep MUST wait only until that deadline.

## The ctx object

Workflows and `eval` cells have `ctx` and `tools` (the same as `ctx.tools`). Plugin functions receive `ctx` as the first argument.

- `ctx.tools.<name>(**kw)` and `ctx.tools["<name>"](**kw)` MUST call the tool. Positional arguments MUST fail. A failed call MUST stop the script.
- `ctx.try_call(op, *args, **kw)` MUST return `struct(ok, value, error)`. It MUST NOT catch a denial, `scope_exceeded`, cancellation, nondeterminism, an interrupted effect, or a step limit.
- `ctx.scope(limit)` MUST accept a limit from 1 to 64. `scope.tools.<name>(**kw)` MUST submit the call and return `None`. At most `limit` calls MUST run at the same time. `scope.all()` MUST wait for every submitted call and return the results in submission order. A failed call MUST raise the first failure in submission order. A scope MUST hold at most 4096 pending calls.
- `ctx.spawn(prompt, runner = None, model = None)` MUST return the subagent id. At most 8 subagents MUST run at the same time. A spawn MUST wait while 8 subagents run. Closing a subagent MUST free its slot. A subagent MUST hold its slot until its turn ends or the subagent stops.
- `ctx.wait(ids)` MUST return a list of final texts.
- `ctx.sleep(seconds)` MUST accept a non-negative finite number.
- `ctx.log(msg)` MUST pass the message to `Log`.
- `ctx.judge(state, questions, model = None)` MUST return a dict from question name to answer. The state MUST be a string, dict, or list. Each question MUST be a dict with `type` `choice`, `noul`, `bool`, or `score`. `bool` MUST be sent as `noul`. A `choice` answer is `struct(choice, probabilities, confidence)`. A `noul` answer is `struct(noul)`. A `score` answer is `struct(score, probabilities, confidence)`. A missing field MUST be `None`.
- `ctx.judge_batch(states, questions, concurrency = 8, model = None)` MUST return one `struct(answers, error)` per state, in input order. `concurrency` MUST be from 1 to 64. An item failure MUST set `error` and MUST NOT raise. Invalid questions, an invalid concurrency, and each stop condition listed for `ctx.try_call` MUST raise. A batch MUST hold at most 4096 states.

## The eval tool

`eval` has class `Script`. Its arguments MUST be `{"code": str, "uses"?: [str], "data"?: any}`. Another field MUST fail with code `invalid_args` and stage `args`.

A cell MUST see `ctx`, `tools`, and `data`. `data` MUST be `None` when absent. A cell MUST NOT keep globals for the next cell. Each call MUST be a durable run with `run_id = call_id`.

A `data` value that cannot decode MUST fail with status `failed`, code `invalid_args`, and stage `args`.

`uses` lists tool names. The allow list MUST be `[eval] uses` when that key is set. Otherwise it MUST be the tool-mode default. A cell without `uses` MUST get the whole allow list. A `uses` entry outside the allow list MUST stop the cell before it runs, with status `denied`, code `scope_exceeded`, and stage `uses`. A call to a tool outside the cell's list MUST stop the cell with status `denied` and code `scope_exceeded`. `uses` does not limit `ctx.spawn`, `ctx.wait`, `ctx.judge`, `ctx.judge_batch`, `ctx.sleep`, or `ctx.log`.

The reply MUST be:

```json
{"invocation_id": "<call id>", "status": "completed", "value": 42, "prints": {"lines": [], "truncated": false}}
```

`status` MUST be one of `completed`, `failed`, `denied`, `cancelled`, `limit_exceeded`, or `indeterminate`. `value` MUST appear only when the status is `completed`. Otherwise `error` MUST appear as `{"code", "message", "stage"}`.

| Code | Status | Stage |
| --- | --- | --- |
| `invalid_args` | `failed` | `args` |
| `syntax` | `failed` | `parse` |
| `scope_exceeded` | `denied` | `uses` or `run` |
| `denied` | `denied` | `run` |
| `cancelled` | `cancelled` | `run` |
| `limit_exceeded` | `limit_exceeded` | `run` |
| `indeterminate` | `indeterminate` | `run` |
| `nondeterminism` | `failed` | `run` |
| `journal` | `failed` | `run` |
| `error` | `failed` | `run` |
| `result` | `failed` | `result` |

`tool.Result.OK` MUST be `true` only for `completed`.

## Tool modes

| Mode | Model-visible tools | Default allow list |
| --- | --- | --- |
| `normal` | every tool | empty |
| `eval-first` | `read`, `search`, `patch` or `apply_patch`, `exec`, `eval`, in this order | the folded tools |
| `eval-only` | `eval` | every tool except `eval` |

`ModelTools` MUST return the visible list. `FoldedTools` MUST return the other tools. The `eval` description MUST end with one line `<name>: <first description line>` per folded tool. `CheckDirect` MUST fail a direct call to a hidden tool with `tool <name> is not available in tool_mode <mode>; call it from eval`. Every tool MUST stay reachable inside cells.

## Workflows

`RunWorkflow` MUST run a file as a durable run. It MUST predeclare `ctx`, `tools`, and `args`, the decoded JSON arguments or `None`. Every tool MUST be reachable and allowed. The engine calls `Completed` with the returned `Outcome`. `Outcome.Reply` MUST encode the reply shape above.

A workflow whose `args` cannot decode MUST fail with status `failed`, code `invalid_args`, and stage `args`.

`dalgona run <file.star> [--arg k=v ...]` MUST read the file in the client and call the daemon method `dalgona/script/run` with `{"entry", "source", "args"?, "cwd"}`. `entry` is the path as typed. `cwd` is the client's working directory. Each `--arg k=v` MUST become member `k` of one `args` object. The value MUST be the JSON value when `v` is valid JSON. Otherwise it MUST be the string `v`. Without `--arg`, `args` MUST be absent and the script MUST see `None`. Another argument shape MUST exit with code 2 and the usage line.

The daemon MUST start a new thread with `cwd` and the configured model, approval, and sandbox. The run id MUST be the thread id. The run MUST execute inside one turn: `turn.started`, then one `user.message` of kind `prompt` with the verbatim text `dalgona run <entry>`, then the run's events, then `turn.completed`. This message marks a workflow turn. No other event kind marks it. The turn MUST NOT call the model, also after recovery. Tool calls MUST pass the approval gate of the thread. A client disconnect MUST interrupt the turn.

Recovery of an open workflow turn MUST first apply the recovery table to each in-flight call under `<run>/`. It MUST then re-run the pinned `script.started` of the run with replay, so recorded effects are not run again. When the run already has `script.completed`, recovery MUST end the turn as `completed` without running it. When the turn has no `script.started` for the run, recovery MUST end the turn as `interrupted`. An open turn with an `assistant.message` is a model turn, even when its prompt starts with `dalgona run `.

The reply MUST be `{"exitCode", "output"}`. `output` MUST hold each print line followed by a newline. For status `completed`, `exitCode` MUST be `0` and `output` MUST end with the result JSON and a newline. Otherwise `exitCode` MUST be `1` and `output` MUST end with `workflow <status>: <message> (<code> at <stage>)`. The client MUST write `output` to stdout and exit with `exitCode`.

## Plugins

`LoadPlugins(home)` MUST load `<home>/plugins/*.star` in name order. It MUST keep every valid file. It MUST return one error per failing file, prefixed `plugin <file>: `.

The top level MAY call only these registration builtins:

- `tool(name, description, params, impl)`: `params` MUST be a dict JSON Schema. `impl(ctx, **args)` runs the tool.
- `command(name, description, impl)`: `impl(ctx, args)` receives the argument string.
- `on(event, impl)`: `event` MUST be `session_start`, `tool_call`, or `turn_end`. `impl(ctx, event)` receives the event payload.

Each effect at load time MUST fail with `effects are not allowed at load time`. A tool or command name MUST be unique across files.

Each plugin call MUST be a durable run. Its source MUST be the defining file. The engine MUST re-execute that source with load-time rules, then call the registered function with the same kind, name, and function name. A plugin tool MUST have class `Script` and `run_id = call_id`. A hook run MUST use `run_id = <runID>/hook:<file>:<fn>`.

`ToolCall` MUST run every `tool_call` hook in load order and MUST return every hook failure joined. A hook failure MUST count as no opinion for that hook only and MUST NOT skip later hooks. A hook MUST return `None`, `{"decision": "allow"}`, or `{"decision": "deny", "reason": str}`. The first `deny` MUST win. Otherwise the first `allow` MUST win. `HookDecision.By` MUST be `hook:<file>:<fn>`. The host MUST journal it as `approval.resolved` with that `by`. Any other result MUST fail with the hook name. `Emit` MUST run `session_start` and `turn_end` hooks and MUST return every hook failure.
