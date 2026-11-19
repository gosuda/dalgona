#### Specifications step 34. Correct `docs/spec/tui.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/tui.md` in one commit. Line numbers are from commit `3c4b6f94`.

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 39 | When the TUI changes thread, it MUST clear the follow-up queue. | internal/tui/model.go:552-557 `func (m *model) setThread(id string)` with `m.followups = nil` when `id != m.threadID`. Depends on the terminal workstream, Step 5 (follow-ups are never cleared silently); re-check this sentence when that  |
| 65 | When the queue holds 32 follow-ups, Alt+Enter MUST show `follow-up queue is full (32); wait for the active turn or press Esc`. | internal/tui/commands.go:158 `m.notice = fmt.Sprintf("follow-up queue is full (%d); wait for the active turn or press Esc", maxFollowups)`; commands.go:21 `maxFollowups = 32`; model.go:201 `return m.queueFollowup()`. Dep |
| 65 | After a follow-up is queued, the notice MUST show `queued follow-up <n> of 32`. | internal/tui/commands.go:163 `m.notice = fmt.Sprintf("queued follow-up %d of %d", len(m.followups), maxFollowups)`. Depends on the terminal workstream, Step 32. |
| 65 | An unknown slash command MUST show `unknown command; use /model /resume /new /compact /login /tree /revert /fork /advisor`. | internal/tui/commands.go:227-228 `default:` then `m.notice = "unknown command; use /model /resume /new /compact /login /tree /revert /fork /advisor"`. Depends on the terminal workstream, Step 23 (every slash command opens a panel); re-c |
| 65 | After `/advisor reset`, the notice MUST show `advisor reset; it reviews the current branch at the next turn`. | internal/tui/commands.go:403 `m.notice = "advisor reset; it reviews the current branch at the next turn"` under `case "advisorReset":`. |
| 79 | A server request whose parameters do not decode MUST receive an error answer. The TUI MUST show `approval request failed: <cause>; reconnect and retry the turn`. | internal/tui/model.go:225-228 `request, err := decodeApproval(msg.method, msg.params)`, then `msg.answer <- requestAnswer{err: err}` and `m.notice = fmt.Sprintf("approval request failed: %v; reconnect and retry the turn" |

Use one word for one thing.

- Keep `option values`. Replace session values at lines 15.

Structure:

- Unchanged and confirmed: Esc answers `cancel` and interrupts the turn (internal/tui/model.go:247-248 `case "esc": decision = "cancel"`, model.go:257-258 `if key == "esc" { return m.interrupt() }`); the audit drift at line 79 is wrong.
- Confirmed: empty model omits model (internal/rpc/protocol.go:39 `json:"model,omitzero"`, internal/tui/commands.go:90 `Model: m.modelName`, model.go:677 `cmp.Or(m.modelName, "default")`).
- Confirmed: follow-up starts only when idle (model.go:415 `if m.state != stateIdle {` returns before the call; commands.go:171 guards running and approval states).
- Confirmed: gradient is a 14-stop palette (internal/tui/keywords.go:138 `[14]string`, keywords.go:140 hue interpolation, keywords.go:141 `hslRGB(h, 0.9, 0.62)`); constants are in keywords.go, not render.go.
- Confirmed: keyword boundary (keywords.go:489 `::` check, keywords.go:511-513 forbiddenPrevious, keywords.go:515-517 forbiddenNext) and forbiddenPeriod (keywords.go:503-508).
- Confirmed: composer tick (model.go:616-635 scheduleShimmer, advanceShimmer with `1.0/14.0`, shimmerVisible requires `m.focused` and a match).
- The term `session` appears otherwise only in the UI label `[a] accept for session` (tui.md:77; model.go:715), which stays.
- Depends on other plan steps: the terminal workstream, Step 10 (keyword glow without motion), 11-12 (live row and status band), 13 (Esc interrupt guard) may change tui.md:25, 31, 79 and 117-125; re-check after they land. The protocol workstream, Step 7 (keyword matches from the server) may replace the TUI matcher text at tui.md:117-119.
- Verified: a failed `initialize` or thread open (`resumed`) sets the notice to `<op> failed: <err>; retry, <hint>, or /new`. Sources: commands.go:55, 62, 64, 272, 278, 282; model.go:175-176; commands.go:348-349 and 458-472. The hint is `/resume` with no open thread and `/resume <id>` otherwise (model.go:562-566).

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 25 | The live view shows these parts in order. They are: streaming items, the approval dialog, an open selector, the notice line, the state line `state: <state>`, the composer, and one status row. |
| 31 | The status row MUST show the sandbox that the server reports in `thread/start`. When the harness sandbox is unavailable, the server reports `dangerFullAccess`. The TUI then shows `no sandbox`, even if the local option is `workspace-write`. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 3 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/tui.md` prints `style findings: 0`, and `just verify` passes.

