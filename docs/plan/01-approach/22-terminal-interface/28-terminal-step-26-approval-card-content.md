#### Terminal step 26. Approval card content

- `approval.go`: kind `panelApproval` (9 rows at 24 rows or more). The card draws through `framePanel` (Step 3).
- Queue (cross-phase decision 26): approvals and asks (Step 27) share one queue in arrival order. Only the front item is drawn. The title counts all pending decisions.
- Title: `Approval · Run command` or `Approval · Apply file changes`, then ` (1 of n)`, then the word `Waiting` in `warning-text`.
- Body: 6 rows. It scrolls with `up` and `down` (`approval.scroll`). Lines: `From thread <id>` when the request comes from another thread; `$ <command>`; `cwd <cwd>` (`internal/rpc/types.go:302-310`); `reason: <reason>`. For file changes: one line per file, `Edited <path> +A −D`, then its diff lines and counts from `item/fileChange/patchUpdated` for that `itemId` (client guarantee G4 of the protocol workstream: the notification arrives before the approval request). The live item with the same `itemId` is the fallback. A change with `diffOmitted` shows `diff omitted, larger than 1 MiB` in place of its diff lines (cross-phase decision 42). When the body is cut, its last row is the overflow line of Step 15.
- Actions: two rows. Row 1 is `[y] accept  [a] accept for session`. Row 2 is `[n] decline  [esc] cancel and interrupt`. `[y] accept` is the point (Design record, The point and the thread hue). It takes the one fill: a background in the thread hue, with the point ink as text colour, or reverse video when colour is off.
- Keys in card mode: `y` accepts. `a` accepts for session. `n` declines at once. `esc` cancels and interrupts (`answerApprovalKey`, `model.go:238-261`). `up` and `down` scroll the body. `ctrl+o` opens the review (Step 15).
- Decline: `n` declines at once, as today (`model.go:238-261` maps `n` to `decline`). The wire response carries only `decision`, so no reason is sent. To redirect the model, the user types a message, which steers the turn (cross-phase decision 44).
- Wire: the decline response carries only the decision today. `internal/rpc/serve_approval.go:120`, `:133` and `:142` call `s.answer(a, agent.ReplyDecline)`, and `decisionReply` at `:152-178` reads `decision` only. No reason member exists in the decline response (cross-phase decision 44).
- `serverRequest/resolved` closes an approval that another window settled. It shows one of these notices, which follow the question notices of the ask workstream, Step 7, with `Approval` in place of `Question`: `Approval answered in another window`, `Approval dismissed in another window`, `Approval closed: the turn was interrupted`, `Approval closed: the agent continues without an answer`.
- Symbols that change: `answerApprovalKey` (`model.go:238`) keeps its keys, and `n` declines at once. `approvalChoices` (`model.go:714-720`) becomes the two action rows. `renderApproval` uses `framePanel` and the body window.
- Failing-first tests:
  - `TestFileApprovalShowsPathsAndDiff` (from the workstream). Input: a file change `a.go` with `+1 −1`. Expected: `Edited a.go +1 −1`, `-old` and `+new` in the card body.
  - `TestApprovalCtrlOOpensReview` (from the workstream). Expected: the review header `Review · ` is shown.
  - `TestApprovalNDeclinesAtOnce` (new). Input: `n`. Expected: one response with `decision` `decline`, no reason member, and the card closes. Source: cross-phase decision 44.
  - `TestApprovalNOpensNoTextMode` (new). Input: `n`. Expected: the input line stays a prompt, and no text mode is active. Source: cross-phase decision 44.
  - `TestApprovalCardSteady` (new). Input: `assertSteadyDock` with 50 `down` presses in card mode. Expected: 12 frame rows (3 plus 9). Source: Step 21 table.
  - `TestModelApprovalPromptAndDecision` (from the workstream, `model_test.go:134`). Expected: `y` still accepts.
- Spec: `docs/spec/tui.md` Approvals.
- Reference: grok-build `crates/codegen/xai-grok-pager/src/views/permission_view.rs:168-182` (via agent://GrokBuildFeatures/report). Taken: capped view height with an expand key. Left: digit picks, scope widening.
- Lands after: Steps 1, 3, 15 and 21.
- Anchor: Edward Tufte

