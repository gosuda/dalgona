### Workstream: Eval default

#### Outcome of the eval-default workstream

Both products start in `eval-first`. The model sees `read`, `search`, `patch` or `apply_patch`, `exec`, `ask`, `ratchet`, and `eval` in top-level threads. It sees `yield` in child threads only. Each name appears only where its thread registers it. A cell reaches every tool except `eval` and `yield`. A cell cannot call `ask`, or `ratchet` with action `approve`, because those calls wait for a person. A cell stops at 1024 tool calls. `dal` gains the `eval` tool, the `tool_mode` key, and the `[eval]` table. `dal` gets no plugins, workflows, hooks, MCP servers, skills discovery, remote runners, or web client. Each step is one commit, and each commit leaves both products working.

#### Decisions of the eval-default workstream

1. Calls that wait for a person stay out of cells. Choice: `waitsForUser(name, action)` refuses `ask`, and `ratchet` with action `approve`, inside `prepareTool`. The message is `tool <name> needs the user and runs only as a direct call`, where `<name>` is `ask`, or `ratchet approve` for `ratchet` with action `approve`. Rival: let cells call them. Reason: a durable cell that blocks on a person holds its replay slot, and its nested call gets an id `<call_id>/<n>` that the window has no item for (Ask decision D10). The owner ruled this on 2026-11-12.

2. One rule for reach and one for the refusal. Choice: reach is decided by tool name in `inCells(name)`, and the refusal is decided per call in `waitsForUser(name, action)`. Both sit in `internal/script/mode.go`. Rival: one predicate for both. Reason: `NewEval` sets reach once per tool name, before any call, so it cannot see the `action` argument. Excluding `ratchet` by name would also block its other actions. The one predicate `waitsForUser` covers `ask` and `ratchet approve`.

3. `ask` and `ratchet` stay reachable names. Choice: `NewEval` keeps them in `names`. Ask step 4 already keeps `ask` in `names`, and this step keeps `ratchet` there too, so that `prepareTool` sends the refusal. Rival: drop them from `names`. Reason: `toolsValue.Attr` (`internal/script/ctx.go:78-82`) would then fail with `tool ask is not reachable from this script`, a plain error that ends with status `failed`, not `denied`. The test `TestScriptCannotCallAsk` of Ask step 4 expects status `denied`.

4. `yield` is never in a cell, and it is a direct call in every mode. Choice: `inCells` excludes `yield`, and the eval-first and eval-only lists hold it. Rival: allow `yield` in cells, as the owner's general rule says. Reason: a nested `yield` ends its child before the cell returns (`internal/agent/spawn.go:242-249`). The owner's three exceptions are `eval`, `yield`, and calls that wait for a person.

5. Cells reach every tool that the eval tool registers, by default. Choice: `DefaultUses` is deleted, and the default allow list is the reach list. Rival: keep `DefaultUses` with one case per mode. Reason: `DefaultUses` has one caller (`internal/script/eval.go:50`), and its `normal` case returns an empty list. `Config.Uses` still narrows the list when `[eval] uses` is set.

6. One cap for every cell call. Choice: `maxCalls = 1024` counts every tool call that passes the refusal and allow checks. Rival: 256 per run for gated calls, or no cap. Reason: the owner ruled that a runaway cell needs a cap. The 256 figure counts gated calls, and the gate in `internal/agent` decides those, so `internal/script` cannot count them. One count of every call is one number in one file. No measurement gates the flip.

7. Three steps, in this order: reach, then dal's eval tool, then the default. Choice: three commits. Rival: flip the default in the commit that adds dal's eval tool. Reason: `ModelTools` drops names that it does not find, with no error (`internal/script/mode.go:27-40`). A default before dal's eval tool would hide `todo` and `web_fetch` from dal with no route to them. Each commit then has one concern.

8. dal gets the script extension through `scripthost.New`, with `Plugins` nil. Choice: `cmd/dal` passes `Batteries: evalBatteries`, which returns only `Extension`. Rival: move the runtime into a core package first. Reason: the owner fixed this wiring. The exact depguard entry `internal/script$` keeps `internal/scripthost` allowed in dal, so no split is needed.

9. dal exposes every ctx builtin that the eval description names. Choice: `ctx.spawn`, `ctx.wait`, `ctx.judge`, and `ctx.judge_batch` come with the same extension, with `Judge: base.Judge`. Rival: expose `tools`, `try_call`, `scope`, `log`, and `sleep` only. Reason: the description constant `evalDescription` in `internal/script/eval.go` names those builtins. `Base.Judge` is set for both products (`internal/app/app.go:81`), and `Thread.Spawn` exists for both (`internal/agent/spawn.go:52`). A narrow set would make the description false in dal.

10. The depguard entry for `internal/script` becomes exact. Choice: the dal entry is `github.com/gosuda/dalgona/internal/script$`, and `internal/app/battery` joins the dal deny list. Rival: delete the entry. Reason: depguard matches an entry without `$` as a prefix, so `internal/script` would also match `internal/scripthost` (depguard v2.2.1, `settings.go`, https://raw.githubusercontent.com/OpenPeeDeeP/depguard/v2.2.1/settings.go). `.golangci.yml` already uses the `$` form at lines 65 and 69.

11. dal accepts `tool_mode` and the `[eval]` table. Choice: both keys leave the dal rejection. Rival: reject `[eval]` in dal. Reason: the owner gives dal the eval tool and the tool modes. `uses` is the only control over what a cell may call. The shared merge loads `[eval]` for both products.

12. The default is set in one place. Choice: `defaultConfigWithHome` (`internal/config/config.go:256-262`) sets `ToolModeEvalFirst`. The zero value of `ToolMode` is the empty string, and `visibleNames` returns nil for it, as it does for `normal`. Rival: read the zero value as eval-first (reader's design choice 7). Reason: literal configs such as the agent harness (`internal/agent/harness_test.go:232-242`) then keep every tool direct, and their tests do not change.

13. `TestLoad_defaults` keeps its assertion and changes its value. Choice: line 56 expects `ToolModeEvalFirst`. Rival: remove the assertion. Reason: removal would leave the dalgona default untested.

14. The default flip has no comparison gate. Choice: step 3 flips the default with no recorded comparison first. Rival: wait for recorded normal and eval-first runs. Reason: the owner overruled that review.

15. No changelog. Choice: README and `docs/spec/config.md` state the new default and the setting that restores `normal`. Rival: a changelog entry (Design and Engineering reviews). Reason: the repository root and `docs/` hold no CHANGELOG file, and the owner did not ask for a new document.

16. Clients do not change. Choice: no client code changes. Rival: fold nested calls in each client. Reason: the tracker emits a nested tool as an item like any other tool (`internal/rpc/serve_track.go:256-266`). It sets the plan on a successful `todo` (`:282-283`). The terminal applies `item/started` through `applyItemStarted` (`internal/tui/model.go:319-320`, `:425`). The web client renders items by type. Live checks confirm the view.

17. Advisor threads, the `CheckDirect` text, and the system prompt do not change. Choice: no change. Rival: none. Reason: advisor threads take fixed tools (`internal/agent/thread.go:168-171`), and the refusal already says to call the tool from `eval`.

18. No new flag. Choice: `tool_mode` and `[eval]` come from the config files. Rival: a `--tool_mode` flag in dal. Reason: the owner named the default, not a flag.

19. Ratchet joins the eval-first direct list in the step that registers it. Choice: Ratchet step 7 adds `ratchetName` to `evalFirstNames`. Rival: add it in this workstream's step 1. Reason: the owner's rule says "in the step that registers it". `internal/script` does not import `internal/ratchet`, because dal reaches `internal/script` through `internal/scripthost`, and the ratchet decision bars dal from importing `internal/ratchet`.

20. The ratchet keyword notice calls `task` through `eval`. Choice: the notice in Ratchet step 8 names the call. Rival: keep the direct wording. Reason: `task` is folded in eval-first, so `CheckDirect` refuses a direct call.

21. The live scenarios that call `todo` and `task` run through eval cells. Choice: the scripted turns `t-plan` (T14, W11) and `t-sub` (T15, W12) call `eval`. `t-ask` (T16, W13) and `t-ratchet` (W22) stay direct, as the owner's rule says. Rival: a second live profile with `tool_mode = "normal"` (reader's design choice 5, option a). Reason: the owner chose the cell route. The scripted turns run at the end of the plan, on the final default.

22. The binary size is measured, not gated. Choice: step 2 records `bin/dal` before and after in its commit message. Rival: a size limit (not asked). Reason: the owner set no limit.

23. The dal visible-tool test lists what `toolSet` registers. Choice: `TestDefaultToolModeShowsOnlyTheEvalFirstList` lists the builtins from `toolSet` plus the eval tool. Rival: a hand-written spec list. Reason: `toolSet` (`internal/app/app.go:147-154`) is what both products register. `dal` registers `web_fetch` too. The thread-level tools `ask`, `yield`, and `task` come from `Thread.withThreadTools` and stay outside this test.

24. `TestToolModeLists` keeps its table. Choice: the new `TestYieldStaysDirect` gives the child case with `yield`. Rival: add a `yield` row to `TestToolModeLists` (reader's design choice 6). Reason: the new test states the behaviour directly.

25. Citation corrections. `RunNestedCall` (`internal/agent/extension.go:87-101`) returns `runNestedCall` (`:100`), which calls `admit` (`:108`). `ctx.spawn` uses `StartChild` (`internal/scripthost/effects.go:375-376`), and `ctx.wait` uses `ChildText` (`:386`). The scripthost harness sets its own mode (`internal/scripthost/harness_test.go:164`), so its tests do not read the config default.


#### Eval default step 1. Cells reach every tool except eval and yield, refuse calls that wait for a person, and stop at 1024 calls

Anchor: Herbert Simon

- Files: `internal/script/mode.go`, `internal/script/eval.go`, `internal/script/ctx.go`, `internal/script/run.go`, `internal/script/eval_test.go`, `internal/scripthost/scripthost_test.go`, `docs/spec/starlark.md`.
- Change:
  - `internal/script/mode.go`: import `github.com/gosuda/dalgona/internal/tool/yield`, the package that `internal/agent/spawn.go:13` imports. Delete `DefaultUses` (`:55-72`). Its only caller is `NewEval` (`eval.go:50`). `evalFirstNames` (`:13`) becomes `[]string{"read", "search", "patch", "apply_patch", "exec", asktool.Name, ratchetName, yield.Name, EvalName}`. `ratchetName` is the constant that Ratchet step 7 adds, and Ratchet step 7 already puts it in this list. Ask step 4 already puts `asktool.Name` in both lists, so this step adds only `yield.Name`. The eval-only case of `visibleNames` (`:21-22`) returns `[]string{asktool.Name, yield.Name, EvalName}`. Replace `DefaultUses` with these two predicates:
```go
func inCells(name string) bool {
    return name != EvalName && name != yield.Name
}

func waitsForUser(name, action string) bool {
    return name == asktool.Name || (name == ratchetName && action == "approve")
}
```
  - `internal/script/eval.go` `NewEval` (`:39-58`): in the loop over `opts.Tools`, keep a spec when `inCells(spec.Name)` is true, instead of `spec.Name != EvalName`. Replace the allow block at `:48-51` with:
```go
allow := names
if opts.Config.Set {
    allow = opts.Config.Uses
}
```
    Ask step 4 keeps `asktool.Name` in `NewEval`, and this step keeps it too, so that the refusal in `prepareTool` names the tool.
  - `internal/script/ctx.go`: add `maxCalls      = 1024` to the const block (`:18-21`), after `maxPending`. `prepareTool` (`:119`) starts with the refusal below. It replaces the `name == asktool.Name` refusal that Ask step 4 adds, and the message for `ask` stays the same.
```go
action := kwString(kwargs, "action")
if waitsForUser(name, action) {
    label := name
    if name == ratchetName {
        label += " " + action
    }
    return Slot{}, nil, r.halt(&stopError{status: StatusDenied, code: codeScope, msg: fmt.Sprintf("tool %s needs the user and runs only as a direct call", label)})
}
```
    The allow check (`:120-122`) stays. After it, insert:
```go
r.calls++
if r.calls > maxCalls {
    return Slot{}, nil, r.halt(&stopError{status: StatusLimitExceeded, code: codeLimit, msg: fmt.Sprintf("the script exceeded %d tool calls", maxCalls)})
}
```
    Add `kwString` after `prepareTool`:
```go
func kwString(kwargs []starlark.Tuple, key string) string {
    for _, kv := range kwargs {
        if name, _ := starlark.AsString(kv[0]); name == key {
            value, _ := starlark.AsString(kv[1])
            return value
        }
    }
    return ""
}
```
  - `internal/script/run.go` `run` struct (`:83`): add the field `calls int`. `toolsValue.bind` calls `prepareTool` on the cell's Starlark thread, so the counter needs no lock.
  - `CheckDirect` (`internal/script/mode.go:75-80`) and `ModelTools` (`:27-40`) keep their form.
- Tests that fail before the change:
  - `TestEvalFirstFoldsToolsIntoEval` (`internal/script/eval_test.go:154-169`): replace the check at `:165-168`. Input: mode `eval-first`, cell `tools.exec(command = 'ls')['tool']`. Expected: value `"exec"`, which `echoTool` (`:33-35`) returns. Before: `scope_exceeded`, because `DefaultUses` gave folded tools only.
  - `TestNormalCellReachesEveryToolWithoutUses` (new, same file): mode `normal`, `Tools: threadTools`, no `Config`. Input: cell `tools.read(path = 'a.txt')['tool']`. Expected: `"read"`. Before: `scope_exceeded`, because `DefaultUses` gave an empty list in `normal`.
  - `TestCellsDoNotReachYield` (new, same file): mode `normal`, specs `read`, `yield`, `eval`. Input: cell `tools.yield(data = 1)`. Expected: status `failed`, code `error`, and a message that contains `tool yield is not reachable from this script`. Source: `toolsValue.Attr` (`internal/script/ctx.go`) returns that text, and `run.failure` gives a plain error the code `error`. Before: `denied` with `scope_exceeded`.
  - `TestCellsRefuseUserCallsAsDirectOnly` (new, same file): mode `normal`, specs `ask`, `ratchet`, `read`, `eval`. Three rows. Cell `tools.ask(questions = [])`: status `denied`, code `scope_exceeded`, message `tool ask needs the user and runs only as a direct call`. Cell `tools.ratchet(action = 'approve', flow = 'x')`: status `denied`, code `scope_exceeded`, message `tool ratchet approve needs the user and runs only as a direct call`. Cell `tools.ratchet(action = 'status', flow = 'x')['tool']`: status `completed`, value `"ratchet"`. Source: decision 1 of this workstream, which fixes the form of the message. Before: row 1 already passes, because Ask step 4 refuses `ask` with this text. Rows 2 and 3 are `denied` with `tool ratchet is not in this script's uses`, so the test fails before this step.
  - `TestYieldStaysDirect` (new, a table of two rows, same file). Row 1: mode `eval-first`, specs `read`, `search`, `patch`, `exec`, `todo`, `web_fetch`, `yield`, `eval`; expected visible names `[read search patch exec yield eval]`. Row 2: mode `eval-only`, specs `read`, `yield`, `eval`; expected `[yield eval]`. In both rows `script.CheckDirect(mode, "yield")` returns nil. Source: the list order in `evalFirstNames` and the eval-only case. Before: `yield` is missing from both lists, and `CheckDirect` refuses it.
  - `TestCellCallCapStopsRunawayLoop` (new, same file): mode `normal`, `Tools: threadTools`, no `Config`. Cell: `for i in range(1025):` followed by the line `    tools.read(path = 'x')`. Expected: status `limit_exceeded`, code `limit_exceeded`, and exactly 1024 calls executed, from `j.counts()`. Source: `maxCalls` in this step, and the `limit_exceeded` row of `docs/spec/starlark.md:116`. Before: status `denied` with `scope_exceeded` on the first call, because `DefaultUses` gives an empty list in `normal`.
  - `TestAskStaysDirectInEveryToolMode` (Ask step 4, same file): this step removes its assertion that `DefaultUses` never contains `ask`, because the function is deleted. Its visible-list assertions stay: eval-first `[read exec ask eval]`, eval-only `[ask eval]`.
  - `TestNestedExecAsksInAskMode` (new, `internal/scripthost/scripthost_test.go`): `h := newHarness(t, func(h *harness) { h.scripts.Mode = config.ToolModeNormal; h.cfg.Config.Approval = config.ApprovalAsk })`. Model queue: `h.model.push(evalCall("tools.exec(command = 'echo hi')"), text("done"))`, then `events := h.idle(h.run("run it"))`. Expected: `decisions(events)` holds one entry, whose key starts with `e/`, with decision `decline`, `By` `mode:ask`, and reason `declined: no client could approve this call`. Source: `declineNoClient` (`internal/agent/gate.go:19`) and `decline("mode:ask", declineNoClient)` (`:270`). Before the test runs, read the risky-command rules in `internal/agent/rules`: the command must match none of them. Before the change: the cell stops with `scope_exceeded`, and no decision exists. Helpers: `evalCall` (`internal/scripthost/harness_test.go:65`), `decisions` (`:316`), `h.scripts` (`:147`), `h.cfg` (`:148`).
- Tests that keep passing: `TestEvalEmptyUsesStopsToolCallWithScopeExceeded` (`eval_test.go:60-70`), `TestEvalUsesOutsideAllowListIsRejectedBeforeRun` (`:72-82`), `TestToolModeLists` (`:117-136`), and `TestCheckDirectRejectsFoldedTools` (`:138-152`).
- Specification text (`docs/spec/starlark.md`):
  - Line 99: replace `Otherwise it MUST be the tool-mode default.` with `Otherwise it MUST be every tool a cell reaches.`
  - Insert a new paragraph after line 99 with these sentences: `A cell MUST NOT reach `eval` or `yield`.` / `A cell MUST stop at its 1025th tool call with status `limit_exceeded` and code `limit_exceeded`. The first 1024 calls MUST run.`
  - Lines 129-131: replace the three rows with `| `normal` | every tool | every tool except `eval` and `yield` |`, `| `eval-first` | `read`, `search`, `patch` or `apply_patch`, `exec`, `ask`, `ratchet`, `yield`, `eval`, in this order, each where the thread registers it | every tool except `eval` and `yield` |`, and `| `eval-only` | `ask`, `yield`, `eval`, in this order, each where the thread registers it | every tool except `eval` and `yield` |`.
  - Line 133: replace the sentences that Ask step 4 writes, "Every tool except `eval` MUST be reachable inside cells. A cell call to `ask` MUST be refused with status `denied` and code `scope_exceeded`. The message MUST be `tool ask needs the user and runs only as a direct call`.", with: Every tool except `eval` and `yield` MUST be reachable inside cells. A cell call to `ask`, or to `ratchet` with action `approve`, MUST be refused with status `denied` and code `scope_exceeded`. The message MUST be `tool <name> needs the user and runs only as a direct call`, where `<name>` is `ask`, or `ratchet approve`. `yield` MUST stay a direct call in every tool mode, because a nested `yield` ends a child before its cell returns.
- Lands after: Ask step 4. Keep `ask` out of Starlark; Ratchet step 7. Battery wiring, top-level threads only

#### Eval default step 2. Give dal the eval tool and the tool modes

Anchor: David Parnas

- Files: `cmd/dal/main.go`, new `cmd/dal/batteries.go`, new `cmd/dal/batteries_test.go`, `internal/config/merge.go`, `internal/config/config_test.go`, `.golangci.yml`, `docs/decisions/two-products.md`, `docs/decisions/dependencies.md`, `docs/spec/config.md`, `README.md`.
- Change:
  - `cmd/dal/main.go:112`: the `app.Options` literal in `startHost` becomes `app.Options{Product: config.ProductDal, Cwd: cwd, Logger: logger, Advisors: advisors, Batteries: evalBatteries}`.
  - New `cmd/dal/batteries.go`:
```go
package main

import (
    "context"

    "github.com/gosuda/dalgona/internal/app"
    "github.com/gosuda/dalgona/internal/scripthost"
)

func evalBatteries(_ context.Context, base app.Base) (app.Batteries, error) {
    return app.Batteries{Extension: scripthost.New(scripthost.Config{Judge: base.Judge, Mode: base.Config.ToolMode, Eval: base.Config.Eval, Logger: base.Logger})}, nil
}
```
    `withDefaults` (`internal/app/app.go:131`) fills the other battery fields, as it does for dalgona.
  - `internal/config/merge.go:530`: `dalgonaOnlyKeys` becomes `map[string]bool{"runner": true, "remote": true, "runners": true, "remotes": true, "mcp_servers": true, "mcp_trusted_workspaces": true, "discovery": true, "herdr": true, "web": true, "edit_review": true}`. `tool_mode` and `eval` leave the map.
  - `.golangci.yml:171-172`: the dal entry becomes `pkg: "github.com/gosuda/dalgona/internal/script$"` with `desc: dal imports the script engine only through internal/scripthost`. After the `codexapp` entry (`:185-186`), add `pkg: "github.com/gosuda/dalgona/internal/app/battery"` with `desc: dal must stay free of dalgona batteries`, indented as the other entries.
  - `docs/decisions/dependencies.md:20`: the last column of the `go.starlark.net` row becomes `scripts, dal eval`.
- Tests that fail before the change:
  - `TestLoad_dalAcceptsToolModeAndEval` (new, `internal/config/config_test.go`): product `dal`, body `tool_mode = "eval-only"`, then an `[eval]` table with `uses = ["read"]`. Expected: no error, `ToolMode` equal to `config.ToolModeEvalOnly`, `Eval.Set` true, and `Eval.Uses` equal to `["read"]`. Source: the spec text of this step. Before: the error `config.toml:1: tool_mode is a dalgona setting`, which `internal/config/merge.go:548` builds.
  - `TestEvalBatteriesCarryOnlyTheScriptExtension` (new, `cmd/dal/batteries_test.go`, package `main`): `t.Setenv("DAL_HOME", t.TempDir())`, then `cfg, err := config.Load(config.ProductDal, t.TempDir(), config.Flags{})`, then `batteries, err := evalBatteries(t.Context(), app.Base{Config: cfg, Logger: slog.New(slog.DiscardHandler)})`. Expected: no error, `Extension` non-nil, and `Tools`, `Skills`, `Runner`, `Attach`, and `Close` nil. Source: the dalgona battery sets those fields in `internal/app/battery/battery.go:73-77`, and this function sets only `Extension`. Before: the test does not compile, because `evalBatteries` does not exist.
- Tests changed: `TestLoad_dalRejectsDalgonaSettings` (`internal/config/config_test.go:218-240`): delete the rows at `:223` (`tool_mode`) and `:230` (`eval`). The other rows keep their text, including the `edit_review` row that Edit-review step 3 adds.
- Specification text:
  - `docs/spec/config.md:7`: replace `` `dal` MUST reject `tool_mode`, `runner`, `remote`, `runners`, `remotes`, `mcp_servers`, `mcp_trusted_workspaces`, `discovery`, `eval`, `herdr`, and `web` as dalgona-only settings. `` with `` `dal` MUST reject `runner`, `remote`, `runners`, `remotes`, `mcp_servers`, `mcp_trusted_workspaces`, `discovery`, `herdr`, and `web` as dalgona-only settings. `tool_mode` and `[eval]` MUST load in both products. ``
  - `docs/decisions/two-products.md`: after the invariant paragraph at line 5, add a paragraph: `The `eval` tool and the tool modes MUST belong to both products. `dal` reaches the Starlark engine only through `internal/scripthost`. Plugins, workflows, hooks, MCP servers, skills discovery, remote runners, and the web client MUST stay with `dalgona`.` At line 15, replace `rejects battery imports from `cmd/dal`.` with `rejects battery imports and the `internal/script` package from `cmd/dal`. `cmd/dal` reaches the eval tool through `internal/scripthost`.`
  - `README.md`: the row `Both products, top level` (`:91`) gains `tool_mode` after `approval`. The row `Dalgona only, top level` (`:100`) loses `tool_mode`. The row `[eval] (dalgona)` (`:105`) loses `(dalgona)`.
- Lands after: Eval default step 1. The key and the test row of Edit-review step 3 are already in the tree, and this step reads both again.

#### Eval default step 3. Make eval-first the default for both products

Anchor: Richard Feynman

- Files: `internal/config/config.go`, `internal/config/config_test.go`, `internal/app/app_test.go`, `docs/spec/config.md`, `README.md`.
- Change:
  - `internal/config/config.go:259`: `ToolMode: ToolModeNormal` becomes `ToolMode: ToolModeEvalFirst`. This is the only place that sets the default.
  - Rule: the zero value of `ToolMode` is the empty string. `visibleNames` returns nil for it, as it does for `normal`, so a literal config keeps every tool direct. The agent harness config (`internal/agent/harness_test.go:232-242`) is such a config, and this step does not change it.
  - `internal/config/config_test.go:56`: `requireConfigValue(t, cfg.ToolMode, config.ToolModeNormal)` becomes `requireConfigValue(t, cfg.ToolMode, config.ToolModeEvalFirst)`.
  - `README.md:109`: after the sentence `Dal rejects the dalgona-only settings.`, add `The default `tool_mode` is `eval-first` in both products. Set `tool_mode = "normal"` to restore the full tool list.`
- Reliance on the old default, found by search: `internal/scripthost/harness_test.go:164` sets its own mode, so its tests do not read the config default. `internal/app/battery/battery_test.go` and `trust_test.go` load the default config but assert only `Tools()` and send no model turn. `internal/app/app_test.go:324` calls `toolSet` directly. None of these changes.
- Direct calls that stop working by default: `todo` and `web_fetch` in both products, and `task` in dalgona, and every MCP tool in dalgona (for example `mcp__docs__lookup`). Each refusal reads `tool <name> is not available in tool_mode eval-first; call it from eval`, from `CheckDirect` (`internal/script/mode.go:75-80`).
- Tests that fail before the change:
  - `TestLoad_defaultToolModeIsEvalFirst` (new, `internal/config/config_test.go`): a table over `config.ProductDal` and `config.ProductDalgona`, each loaded with `loadConfig(t, product, "", "", config.Flags{})` as `TestLoad_defaults` does. Expected: `cfg.ToolMode == config.ToolModeEvalFirst`. Before: `normal`.
  - `TestDefaultToolModeShowsOnlyTheEvalFirstList` (new, `internal/app/app_test.go`, package `app`): a table over both products. For each product, `t.Setenv` sets `DAL_HOME` or `DALGONA_HOME` to `t.TempDir()`, and `cfg, err := config.Load(product, t.TempDir(), config.Flags{})`. Then `builtin, err := toolSet(cfg, func(context.Context, string) []tool.Tool { return nil })(ctx, "openai/gpt-test", cwd)`. Then `ext := scripthost.New(scripthost.Config{Mode: cfg.ToolMode})(nil)`. The factory ignores its thread in `Tools` and `Visible`. The test passes `ext.Tools(builtin)` through `ext.Visible`, and compares the names. Expected: `[read search patch exec eval]`. Source: the builtins at `internal/app/app.go:149-152`, the default edit style `anchor`, whose tool is `patch`, and the eval tool that `internal/scripthost/scripthost.go:51` appends. The thread-level tools `ask`, `yield`, `task`, and `ratchet` come from `Thread.withThreadTools` and are outside this test. Before: all seven names (`read`, `search`, `patch`, `exec`, `todo`, `web_fetch`, `eval`).
  - `TestLoad_defaults` (`internal/config/config_test.go:48-60`): line 56 now asserts the new value. Before the change, that assertion fails once the default flips.
- Specification text:
  - `docs/spec/config.md:9`: replace `The default tool mode is `normal`.` with `The default tool mode is `eval-first` for both products. `tool_mode = "normal"` restores the full tool list.`
- Lands after: Eval default step 2

#### Schemas of the eval-default workstream

- `inCells(name string) bool` and `waitsForUser(name, action string) bool`, in `internal/script/mode.go`. `ratchetName` is the constant `"ratchet"` that Ratchet step 7 adds to the same file.
- `kwString(kwargs []starlark.Tuple, key string) string`, in `internal/script/ctx.go`. It returns the string value of the first keyword argument named `key`, or the empty string.
- `maxCalls = 1024`, in `internal/script/ctx.go`, and the field `calls int` on `run`.
- `visibleNames(mode config.ToolMode) []string`: `normal` returns nil. `eval-first` returns `read`, `search`, `patch`, `apply_patch`, `exec`, `ask`, `ratchet`, `yield`, `eval`. `eval-only` returns `ask`, `yield`, `eval`. `ModelTools` drops the names a thread does not register.
- `NewEval(fx Effects, opts EvalOptions) *Eval` keeps its signature. `Config.Uses` narrows the allow list only when `Config.Set` is true.
- `evalBatteries(_ context.Context, base app.Base) (app.Batteries, error)`, in `cmd/dal/batteries.go`, returning only `Extension`.
- `dalgonaOnlyKeys` after this workstream: `runner`, `remote`, `runners`, `remotes`, `mcp_servers`, `mcp_trusted_workspaces`, `discovery`, `herdr`, `web`, and `edit_review` (added by Edit-review step 3).
- Error texts that stay: `tool %s is not available in tool_mode %s; call it from eval`, `tool %s is not reachable from this script`, `tool %s is not in this script's uses`, and `declined: no client could approve this call`. New texts: `tool %s needs the user and runs only as a direct call` and `the script exceeded %d tool calls`.

#### Checks of the eval-default workstream

- Each step runs `just verify`, which covers the Go tests and `golangci-lint` with the depguard rules.
- After each step, run `uv run --no-project --python 3.12 docs/spec/check_style.py <file>` on each changed spec file. It must report no new findings.
- Step 1: run `go test ./internal/script/ -run 'TestEval|TestNormalCell|TestCells|TestCellCall|TestYield|TestAsk'`, `go test ./internal/scripthost/ -run TestNestedExecAsksInAskMode`, and `go test ./internal/scripthost/ ./internal/agent/ -run 'Spawn|Child'`. A spawn test that expected a missing `yield` in eval-only now sees the yielded result.
- Step 2: run `go test ./internal/config/ -run TestLoad_` and `go test ./cmd/dal/ -run TestEvalBatteries`. Add a scratch import of `github.com/gosuda/dalgona/internal/script` to `cmd/dal`; `just lint` must fail. Replace it with `internal/scripthost`; `just lint` must pass. Revert the scratch edit. Build `./cmd/dal` before and after this step, and record both sizes in the commit message.
- Step 3: run `go test ./internal/config/ -run 'TestLoad_defaultToolModeIsEvalFirst|TestLoad_defaults'` and `go test ./internal/app/ -run TestDefaultToolModeShowsOnlyTheEvalFirstList`. The list covers `toolSet` only, so the Ask steps 4 and 5 and Ratchet step 7 do not change its expected names.
- Live scenarios for both products are in the outside checks.

#### Prerequisites of the eval-default workstream

- The tree is at commit `3c4b6f94` or later, with Ask steps 4 and 5 and Ratchet steps 6 to 8 landed. Re-read each cited line before editing it, because later steps may have moved it.
- Specifications step 1 creates `docs/spec/check_style.py`. Specifications steps 6, 33, and 39 edit `docs/spec/config.md`, `docs/spec/starlark.md`, and `docs/decisions/two-products.md`. Re-read the lines that each one names.
- Edit-review step 3 adds `edit_review` to `dalgonaOnlyKeys` and a row to `TestLoad_dalRejectsDalgonaSettings`. Eval default step 2 edits the same map and table, and its author re-reads both first.

