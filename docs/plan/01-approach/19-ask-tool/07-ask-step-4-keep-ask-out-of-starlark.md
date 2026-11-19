#### Ask step 4. Keep `ask` out of Starlark

Files:

- `internal/script/mode.go`:
  - `evalFirstNames` (`:13`) becomes `{"read", "search", "patch", "apply_patch", "exec", asktool.Name, EvalName}`.
  - `ToolModeEvalOnly` in `visibleNames` (`:21-22`) returns `{asktool.Name, EvalName}`.
  - `DefaultUses` (`:55-72`) skips `asktool.Name` the same way it skips `EvalName`.
- `internal/script/eval.go`: no change. `NewEval` (`:39-58`) keeps `asktool.Name` in `reach`, so `toolsValue.Attr` (`internal/script/ctx.go:78-82`) finds it, and `prepareTool` sends the refusal.
- `internal/script/ctx.go`: `prepareTool` (`:119-135`) first returns `r.halt(&stopError{status: StatusDenied, code: codeScope, msg: "tool ask needs the user and runs only as a direct call"})` when `name == asktool.Name`. This covers workflows and plugins, whose `runConfig{}` reaches every tool (`plugin.go:194`, `workflow.go:25`).

Tests in `internal/script/eval_test.go`, package `script_test`. Both fail before this step.

- `TestAskStaysDirectInEveryToolMode`: with `specs("read","exec","ask","web_fetch","eval")`:
  - eval-first visible is `[read exec ask eval]`;
  - eval-only visible is `[ask eval]`;
  - folded lists never contain `ask`;
  - `DefaultUses` never contains `ask`.
- `TestScriptCannotCallAsk`: `script.NewEval(j, script.EvalOptions{Mode: config.ToolModeNormal, Tools: specs("ask", "eval"), Config: config.EvalConfig{Uses: []string{"ask"}, Set: true}})` with `j := newJournal(echoTool)`. The cell `tools.ask(questions = [])` stops with status `denied`, code `scope_exceeded`, and the message `tool ask needs the user and runs only as a direct call`. Source: the refusal text of this step. Before: the allow check passes and `echoTool` answers, so the status is `completed`.

The existing `TestToolModeLists` (`:117-136`) keeps its table, because `threadTools` has no `ask`.

Docs: `docs/spec/starlark.md` `## Tool modes`:

- The visible lists for eval-first and eval-only gain `ask`.
- The sentence "Every tool MUST stay reachable inside cells." becomes these sentences: Every tool except `eval` MUST be reachable inside cells. A cell call to `ask` MUST be refused with status `denied` and code `scope_exceeded`. The message MUST be `tool ask needs the user and runs only as a direct call`.

- Lands after: Step 1 (the `asktool.Name` constant).

Anchor: Herbert Simon.

