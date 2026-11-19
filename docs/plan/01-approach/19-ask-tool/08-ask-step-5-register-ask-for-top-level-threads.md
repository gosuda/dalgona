#### Ask step 5. Register `ask` for top-level threads and recover it

Files:

- `internal/agent/spawn.go`: rename `withSubagentTools` (`:39-50`) to `withThreadTools` and update its only caller, `thread.go:267`. New body:

```go
func (t *Thread) withThreadTools(tools []tool.Tool) []tool.Tool {
	if t.created.Advisor != "" {
		return tools
	}
	if t.created.Parent != "" {
		return append(tools, yield.New())
	}
	tools = append(tools, asktool.New(t))
	if t.host.cfg.Product == config.ProductDalgona {
		return append(tools, task.New(t))
	}
	return tools
}
```

  Advisor threads with `fixedTools` never reach this function (`thread.go:260-262`), and `withThreadTools` returns early for other advisor threads (`spawn.go:40-41`).
- `internal/agent/recover.go`: the first statement of `recoverStarted` (`:131`; its body starts at `:132`) becomes `if state.Tool == asktool.Name { return w.completeRecovered(ctx, state.CallID, asktool.TextRestarted, recoveredStopped) }`. `completeRecovered` (`recover.go:182-185`) leaves `OK` unset, so the journal records `ok=false`.

Tests:

- `internal/agent/ask_tool_test.go`:
  - `TestAskToolAnswerReachesModel`: `h.model.push(calls(call{id: "q1", name: "ask", args: <the answered example of the section "Schemas of the ask workstream">}), text("done"))`. It subscribes, reads `Question.RequestID` from the `UpdateQuestion` update, answers through `AnswerQuestion` with `asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{"Postgres"}, {"Search", "Export"}, {"ledger-svc"}}}`, then checks that `callOutputs(h.model.request(1))["q1"]` equals the exact answered text.
  - `TestAskToolDeclinesWithoutClient`: the output equals `TextNoClient`.
  - `TestAskToolOnlyOnTopLevelThreads`:
    - the parent's first request lists `ask`;
    - a `task` child request (routed with `newChildScript`, `subagent_test.go:28-55`) does not list it;
    - a child call to `ask` completes with `Tool ask not found` (`tools.go:209`).
  - `TestAskToolInDal`: `newHarness(t, func(h *harness) { h.cfg.Config.Product = config.ProductDal })` lists `ask`.
- `internal/agent/recover_test.go`: add `TestRecoverInterruptsOpenAsk`, seeded like `TestRecoverRerunsReadInFlight` (`:119-125`). It expects `ok=false`, `recovered=interrupted`, output `TextRestarted`, and the same text in the next model request.
- `internal/rpc/serve_input_test.go`: add `TestServeAskToolEndToEnd`. It pushes `sseCall("call-q", "ask", args)` and `sseText("done")`, answers the request, then checks `findItem(readTurn(...).completed, "call-q").Output`.
- The advisor test `TestRuntime_advisorRequestUsesRoleModelMediumEffortAndOnlyGrantedTools` (`internal/agent/advisor/runtime_test.go:261`; its tool assertion is at `:283-285`) already pins advisor tools to `[advise read]`, so it proves the advisor lacks `ask`.

Every new test fails before this step, because no thread has the tool.

Docs:

- `docs/spec/recovery.md`, section `## Rules`: this step adds one row to the table, before the row that starts with `tool.started`, class `read`. The edit-review workstream, Steps 4 and 4a change rows and add a rule here. This step does not edit them. The new row: "`tool.started` for `ask` | Journal `ok=false`, `recovered=interrupted`, output `interrupted: the host restarted before the user answered`. The question MUST NOT be asked again."
- `docs/spec/ask.md`: `## Scope`, covering both products, top-level threads only, why subagents and advisors are excluded, and the enforcement.

- Lands after: Steps 2 and 3.

Anchor: Dieter Rams, Tony Hoare.

