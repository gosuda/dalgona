# Tools, modes, and approval

Modes: `normal` gives `read`, `search`, `patch`, `exec`; `eval-first` adds `eval`; `eval-only` leaves only `eval`. Each tool takes typed parameters with limits. Edit styles: `anchor` by default; per-model table picks `replace`, `apply_patch`, `anchor`, `hashline`; tiers `simple`, `balanced`, `strict`. Search modes include symbol search and AST edit; no LSP. The approval ladder `--approval ask|edits|all` carries ask, edits, all; by default dal asks before every `patch` and every `exec`; when no person can answer, dal denies and says why. Call-scoped argv-prefix grants allow one command prefix. Background jobs run detached; subagents are off by default in dal. Read schemes: `dal://`, plus plugin schemes.

## The eval tool

`eval` runs one Starlark cell per call. Nothing but Starlark runs in it, and every effect a cell requests goes through the same approval as the tool of that name. A call you deny outside `eval` is denied inside it. `eval` is not a sandbox.

The request has one required field and two optional ones, and nothing else:

```json
{"code": "tools.search(\"TODO\", path = \"src\")", "uses": ["tools.search"], "data": {"limit": 20}}
```

- `code` is the cell.
- `uses` narrows the operations the cell may request.
- `data` is any JSON value. The cell reads it as `data`; it is `None` when absent.

A cell inherits its operations from `eval.uses` in `dal.toml`. Leave out `uses` and the cell may request all of them. Give `uses` and the cell may request only those, and each must be in `eval.uses`; an id outside it stops the cell with `scope_exceeded`. Give `uses = []` and the cell can only compute: no tools, network, state, or child sessions. With no `eval.uses`, every cell can only compute.

Each cell starts fresh. It has three names: `ctx`, `tools` (the same as `ctx.tools`), and `data`. It needs no `load` and no wrapper function, and it keeps no globals for the next cell. The last expression is the result. If the last statement is not an expression, the result is `None`. There is no `return`.

```starlark
page = tools.search("TODO", path = "src")
{"count": len(page.matches), "truncated": page.truncated}
```

Calls return their values directly, and a failed call stops the cell. To inspect an expected failure, pass the operation itself to `ctx.try_call`, not the result of calling it:

```starlark
r = ctx.try_call(tools.exec, "cargo test")
{"exit_ok": r.ok}
```

Denial, cancellation, and hard limits stop the cell and cannot be caught. A patch that ran before a later failure is not undone.

A pure cell reads only its data:

```starlark
sorted(data["names"])
```

Send it with `"uses": []`. To call a plugin tool from a cell, write `tools["quality.todos"](path = "src")`; the cell needs the tool's id and the operations it declares, as the `plugins` page explains. To run calls at the same time, open a scope with a limit from 1 to 64, then collect the results in submission order:

```starlark
s = ctx.scope(limit = 2)
for path in ["src", "tests"]:
    s.tools.search("TODO", path = path)
s.all()
```

The reply is an envelope, not just the value:

| field | meaning |
|---|---|
| `invocation_id` | the id of this cell run |
| `status` | `completed`, `failed`, `denied`, `cancelled`, `limit_exceeded`, or `indeterminate` |
| `value` | the result; present only when `status` is `completed` |
| `error` | `code`, `message`, and `stage`; present when the cell did not complete |
| `prints` | the lines the cell printed and whether they were cut; a diagnostic channel only |
| `cleanup` | whether dal finished cleanup, and the ids of any calls still outstanding |
| `observer_errors` | hooks that failed while watching the cell |

A `completed` status means the code finished. It does not mean the goal was met, that a test passed, or that every handled failure succeeded.
