#### Terminal step 23. Every remaining slash command opens a panel

- `/new`: a confirm panel (`panelConfirm`, the mechanism of Step 4) titled `Start a new thread?`. Body line 1 is `N follow-ups move to it` when the queue is not empty. Actions `[enter] start` and `[esc] cancel`.
- `/compact`: a confirm panel titled `Compact the context?`. Body line 1 is `ctx <pct>% of <window> tokens` when `thread.usage.ModelContextWindow` is known. `<pct>` is the context percent of Step 12. Actions `[enter] compact` and `[esc] cancel`.
- `/fork`: a list panel of the active-path turns from `dalgona/thread/tree`, newest first. `enter` calls `thread/fork` with `rpc.ThreadForkParams{ThreadID, LastTurnID}` (`internal/rpc/protocol.go:73-76`).
- `/advisor`: a list panel from `dalgona/advisor/status`, showing name, status, model, notes and tokens. `enter` toggles the highlighted advisor with `dalgona/advisor/set`. `ctrl+r` opens a confirm panel titled `Reset the advisor?` with actions `[enter] reset` and `[esc] keep`. `enter` on it calls `dalgona/advisor/reset`. `ctrl+r` is used because printable keys edit the filter (Step 21). Error -32001 shows the `advisorFailure` text in the list's hint row.
- `/review` (new): a list panel titled `Review changes` with the rows `Uncommitted changes`, `Base branch`, `Commit` and `Custom`. `enter` on `Uncommitted changes` calls `review/start` at once. On the other three rows, `enter` sets `listPanel.pending` to `branch`, `commit` or `instructions`. The input line then takes the text (`inputTarget`, Step 21), and `enter` sends the call.
- Wire: `review/start {threadId, target}`, with the target forms of the protocol workstream, Step 14: `uncommittedChanges`, `baseBranch{branch}`, `commit{sha, title?}` and `custom{instructions}`. Response `{reviewThreadId, turn}`. The errors `invalid branch <b>`, `invalid sha <s>` and `instructions must not be empty` show in the list's hint row. The review starts as a turn on the given thread (the protocol workstream, Step 14), so the client switches nothing.
- `/review` is refused while a turn runs, with the notice of Step 6. `/review` joins the commands that Step 6 refuses. The guard keeps a second turn from being sent from this client.
- `slash.go`: `func (m *model) reviewStart(target, text string) tea.Cmd` builds the target and calls `review/start`.
- The argument forms `/model id`, `/resume id` and `/advisor status` stay direct.
- Symbols that change: `commands` (Step 7) gains `review`. `busyRefusal` (Step 6) names `review`. `reviewStart` is new in `slash.go`.
- Failing-first tests:
  - `TestNewOpensConfirmPanel` (from the workstream). Input: `/new` with a queue of 2. Expected: title `Start a new thread?` and body `2 follow-ups move to it`. Source: this step.
  - `TestForkPanelSendsLastTurnId` (from the workstream). Expected: `LastTurnID` equals the highlighted turn.
  - `TestAdvisorPanelToggles`. Input: `enter` on a disabled advisor. Expected: one `dalgona/advisor/set` call with `enabled` true. Source: this step.
  - `TestAdvisorPanelShowsUnavailable` (from the workstream). Input: error -32001. Expected: the hint row shows the `advisorFailure` text.
  - `TestReviewUncommittedSendsTarget` (new). Input: `/review`, then `enter`. Expected: one call to `review/start` with the followed thread id and target `uncommittedChanges`. Source: the protocol workstream, Step 14.
  - `TestReviewBaseBranchTypedInInputLine` (new). Input: `/review`, `down`, `enter`, the text `main`, `enter`. Expected: one call with target `baseBranch` and branch `main`. The view contains `branch: main`. Source: the target forms above.
  - `TestReviewRefusedMidTurn` (new). Input: `/review` while the state is running. Expected: no call. The notice reads `/review is unavailable while a turn is working; wait for it to finish or interrupt it first`. Source: Step 6 text.
  - `TestAdvisorDockSteady` (new). Input: `assertSteadyDock` with 50 `down` presses. Expected: 15 frame rows. Source: Step 21 table.
  - `TestModelSlashCommands` and `TestModelAdvisorCommands` (from the workstream) move to the panels. Their expected values follow the rules above.
- Spec: `docs/spec/tui.md`, its rows in the keys table and in Panels (Step 21 owns the section).
- Lands after: Steps 4, 6, 7 and 21.
- Anchor: Christopher Alexander

