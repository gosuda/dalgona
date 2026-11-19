#### Specifications step 2. Correct `docs/spec/advisor.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/advisor.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 3 | The advisor MUST NOT change primary session state, except when it delivers a note. | The advisor MUST NOT change primary thread state, except when it delivers a note. | docs/spec/advisor.md:3 `primary session state`; `thread` is the protocol unit served by internal/rpc/serve.go:26 `"thread/list"` |
| 77 | The runtime MUST be a child session in the journal: | The runtime MUST be a child thread in the journal: | internal/agent/gate.go:252 `if w.t.created.Advisor != ""` (the advisor is a child thread with `created.Advisor` set); internal/agent/event/event.go:13 `TypeSessionCreated = "session.created"` (literal kept) |
| 79 | The runtime MUST create the advisor session at its first review. An existing advisor session MUST be resumed with the advisor tools and prompt. `Runtime.Close` MUST remove the boundary hook, stop the review loops, and close every advisor session. A later `Attach` MUST then resume the session with its own tools. When `Attach` fails, it MUST close every advisor session that it opened. | The runtime MUST create the advisor thread at its first review. An existing advisor thread MUST be resumed with the advisor tools and prompt. `Runtime.Close` MUST remove the boundary hook, stop the review loops, and close every advisor thread. A later `Attach` MUST then resume the thread with its own tools. When `Attach` fails, it MUST close every advisor thread that it opened. | internal/agent/advisor/runtime.go:61 `func Attach(ctx context.Context, host *agent.Host, thread *agent.Thread, cfg config.Config)` (advisor is a `*agent.Thread`); internal/agent/host.go:172-180 `AllThreads` (advisor entries are host threads) |
| 85 | A fork without an advisor session MUST start its cursor at 0. | A fork without an advisor thread MUST start its cursor at 0. | internal/agent/advisor/review.go:453 `a.cursor, a.failures = cursor, 0` (cursor reset path); term only |

| 109 | Input that arrives while the `turn_end` hook runs MUST start a normal turn when auto-continue is on, and MUST NOT start a turn when auto-continue is off. | Input that arrives while the `turn_end` hook runs MUST start a normal turn when the turn completed and auto-continue is on. | internal/agent/entry.go:132-133 `_, err := a.append(message)` / `a.autoContinue = a.autoContinue \|\| err == nil` (any accepted busy input sets auto-continue on before settlement); internal/agent/turn.go:261 `w.notifyTurnEnd(ctx, o.status)` runs before internal/ |
| 136 | The note text MUST escape `&`, `<`, and `>`. | The note text MUST escape `&`, `<`, `>`, `"`, and `'`. | internal/agent/event/event.go:612-613 `func (e AdvisorNote) Render() string` / `return `<advisory advisor="` + html.EscapeString(e.Advisor) + ... + html.EscapeString(e.Note) + `</advisory>`` |
| 177 | on the advisor session active path after its last compaction. | on the advisor thread active path after its last compaction. | internal/agent/host.go:177 `if info.Advisor == ""` (advisor threads are host threads); term only |
| 179 | The TUI MUST render a note as a dim card `advisor · <severity>: <note>`. | The TUI MUST render a note as a dim card. For the advisor named `advisor`, the card reads `advisor · <severity>: <note>`. For any other advisor, the card reads `advisor <name> · <severity>: <note>`. | internal/tui/render.go:45-49 `label := "advisor"` / `if advisor != "" && advisor != label { label += " " + advisor }` / `return faint.Render(wrapTerminal(label+" · "+severity+": "+note, m.width))` |
| 183 | A `task` call opts in when its caller passes a configuration with `Subagents` set. | A `task` call opts in when its `advisor` argument is `true`. | internal/tool/task/task.go:64-65 `case "advisor":` / `req.Advisor, err = model.ReadBool(decoder)`; internal/agent/spawn.go:108-113 `childAdvisorConfigured` (reads config only); internal/app/app.go:185-186 `if req.Advisor { attach = a.registry.AttachChild }`; i |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 115 | An `advise` call whose arguments exceed 64 KiB MUST fail with the error `advise: note is too long`. | internal/agent/advisor/advise.go:44-45 `if len(call.Args) > event.MaxInlineBytes { return tool.Result{}, errors.New("advise: note is too long") }`; internal/agent/event/event.go:36 `const MaxInlineBytes = 64 << 10` |
| 175 | `dalgona/advisor/reset {threadId}` MUST clear the halted state and the failure count of the thread's advisors. The `status`, `set`, and `reset` methods MUST attach the thread's advisor runtime when it is not attached. | internal/rpc/serve.go:48-50 `"dalgona/advisor/status"`, `"dalgona/advisor/set"`, `"dalgona/advisor/reset": (*server).advisorReset`; internal/rpc/serve_advisor.go:32-40 `advisorFor` (`Get`, then `s.open` and `ensureAdviso |
| 177 | The `usage` object MUST contain exactly `inputTokens` and `outputTokens`. | internal/rpc/types.go:190-193 `AdvisorUsage struct { InputTokens int `json:"inputTokens"`; OutputTokens int `json:"outputTokens"` }` |

Use one word for one thing.

- Keep `thread`. Replace session at lines 3, 77, 79, 85, 89, 113, 177.

Structure:

- Keep the literal `session.created` on line 77 (journal event name, internal/agent/event/event.go:13).
- Resolved, no change (line 77, `thread/list` must not list the advisor): internal/agent/host.go:177 filters advisor threads inside `AllThreads`; internal/rpc/serve_thread.go:430 `listThreads` uses `AllThreads`.
- Resolved, no change (line 113, advisor tools use the advisor's own snapshot store): internal/agent/thread.go:116 `snapshots: snapshot.New()` is per thread; internal/agent/tools.go:391 `Snapshots: w.t.snapshots` runs on the advisor thread.
- Resolved, no change (line 166, shared retry rules): internal/app/providers.go:168 `retry := model.DefaultRetryOptions()` is passed to every provider at :172-185; internal/agent/advisor has no retry option outside tests.
- Resolved, confirmed (line 109 fix above). Line 113 term change and sentence rewrite are in the list of long sentences below.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 89 | The review prompt MUST be submitted with `Input.Verbatim`. Keywords and `@path` mentions in the primary transcript MUST add no notice, no attachment, and no effort change. |
| 109 | Input that arrives while the `turn_end` hook runs MUST start a normal turn when the turn completed and auto-continue is on. |
| 113 | An advisor thread has no approval client. It MUST NOT wait for a user decision. In `ask` mode, its calls fall back to the configured decider. When no decider is configured, its calls decline. |
| 119 | `Normalize(note)` MUST apply NFKC, then lowercase. It MUST then replace each run of characters that are not Unicode letters or numbers with one space. It MUST then trim the ends. |
| 130 | Displacement: when the budget is full, a non-blocker note MAY replace the lowest-severity pending note of the current review. It MAY do so only when its severity is strictly higher. |
| 162 | A wait MUST end when the review ends with any outcome. A wait MUST also end when the advisor halts or is disabled. A wait MUST also end when the primary turn is interrupted or the runtime closes. |
| 168 | The output of a review is unsafe when an assistant text of the review contains a destructive shell directive. A directive that the review input also contains does not make the output unsafe. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 7 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/advisor.md` prints `style findings: 0`, and `just verify` passes.

