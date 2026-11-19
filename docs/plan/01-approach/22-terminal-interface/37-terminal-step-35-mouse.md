#### Terminal step 35. Mouse

- `update.go` `View`: `view.MouseMode = tea.MouseModeCellMotion` only while the review or a list panel is open. Otherwise `view.MouseMode = tea.MouseModeNone` (Bubble Tea practice rule 3). Only `View` sets it.
- `tea.MouseWheelMsg` with `MouseWheelUp` or `MouseWheelDown` changes the `scroll` of the open list panel by 3 rows, clamped to the list. If the cursor leaves the visible rows, it moves to the nearest visible row (cross-phase decision 47). The review receives the same message (Step 15). Other panels ignore the wheel.
- A click selects nothing (cross-phase decision 47). The client cannot learn the screen row of the dock.
- Symbols that change: `View` (`update.go`), the wheel case of `updateAsync`. No new symbol.
- Failing-first tests:
  - `TestMouseModeOnlyWithOverlay` (new). Input: open the resume list, then the approval card, then the review, then close each. Expected: `MouseModeCellMotion` for the list and the review, and `MouseModeNone` for the approval card and for the closed dock. Source: Bubble Tea practice rule 3.
  - `TestWheelScrollsPicker` (from the workstream). Input: a wheel-down message on a list of 300 rows. Expected: the window moves 3 rows.
  - `TestClickSelectsNothing` (new). Input: a click on row 3 of a list of 300 rows. Expected: `cursor` and `scroll` are unchanged. Source: cross-phase decision 47.
- Spec: `docs/spec/tui.md`, new section `Mouse`.
- Reference: omp2 `crates/chat/src/host.rs:4089-4090` (via agent://Omp2Features/report). Taken: opt-in only while an overlay needs the pointer.
- Lands after: Steps 3, 15, 21 and 26.
- Anchor: Christopher Alexander

