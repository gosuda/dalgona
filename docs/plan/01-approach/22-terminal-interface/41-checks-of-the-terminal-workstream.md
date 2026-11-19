#### Checks of the terminal workstream

- Per step: `go test -race -count=1 ./internal/tui/...`, then `just verify` (`justfile:121`) before the commit.
- Import rule: `go tool golangci-lint run ./internal/tui/...` (`depguard` block, `.golangci.yml:59`).
- Invariant tests: `go test -race -count=1 -run 'TestInv' ./internal/tui/`. Every test below fails before the change, because the symbols it calls (`dockLines`, `renderBand`, `cells`, `markKeywords`, the review and notify functions) do not exist yet, or because the old behaviour is still present. Expected values come from the design table and from the steps named. They are derived, not run.

| Id | Test | File | Input | Expected |
|---|---|---|---|---|
| I1 | `TestInvI1PrintedRowsOnlyGrow` | `frame_test.go` | A scripted session of 12 messages (keys, ticks, notices, item deltas, item completions) at 100 by 24 | After each message the keys of `m.out.entries`, in order, extend the previous list, and no entry text changes |
| I2 | `TestInvI2DockHeightChangesOnlyOnKey` | `frame_test.go` | Ticks, deltas, notices, turn start and end, hint expiry, plan change | `len(dockLines())` is unchanged after each. A typed key, a paste, a panel open or close, and a window resize may change it (cross-phase decision 52) |
| I3 | `TestInvI3BandFieldsKeepColumns` (step 12) and `TestInvI3LiveRowFieldsKeepColumns` (step 11) | `band_test.go`, `dock_test.go` | Percent 5 and 100, queue 1 and 12, effort high and low; elapsed 12 s and 2 m 05 s | The start column of every item and of the activity text is the same in both renders |
| I4 | `TestInvI4NoClearOrResetBytes` | `program_test.go` | `tea.WithOutput(&buf)`, `tea.WithInput`, `tea.WithWindowSize(100, 24)`, `tea.WithColorProfile`, `tea.WithoutSignals`; a turn, a resize to 40 and back, a review open and close | `buf` holds none of `\x1b[2J`, `\x1b[3J`, `\x1bc`, `\x1b[H`, `\x1b[1;1H`, and no `\x1b[?1049h` outside the review |
| I5 | `TestInvI5EachItemPrintedOnce` | `program_test.go` | The run of I4, with one item completion sent twice and one resume replay | The row text appears once in `buf` |
| I6 | `TestInvI6FrameBounds` | `frame_test.go` | Every frame of the scripted session at 40 by 8 and 100 by 24 | `len(lines) <= height - 1` and `cells(line) <= width - 1` |
| I7 | `TestInvI7NarrowingPrintsNothing` | `frame_test.go` | A `tea.WindowSizeMsg` from 100 to 40 during a turn | `m.out.printing` is false, `m.out.settling` is true, and `m.out.printed` is unchanged |
| I8 | `TestInvI8TickChangesOnlyFixedFields` | `frame_test.go`, `program_test.go` | Two frames one tick apart; a tick with no state change | The frames differ only in the dot and elapsed columns. The tick with no change adds 0 bytes to `buf` |
| I9 | `TestInvI9ScreenMethodFollowsRenderer` | `width_test.go` | A `tea.ModeReportMsg{Mode: ansi.ModeUnicodeCore, Value: ansi.ModeReset}` sent to the model, then the family emoji U+1F468 U+200D U+1F469 U+200D U+1F467 measured by `cells` | After the report, `screen.method` is `ansi.GraphemeWidth` and the emoji is 2 cells. Under `ansi.WcWidth` the same emoji is 6 cells |

The byte-stream checks use `tea.WithOutput` into a buffer and search it. The module cache has no terminal emulator, so the screen checks for I1 and I7 run in the live session as well (below).

Live checks and the scripted recipe belong to the live-debug workstream: rows T35 to T45 of `docs/spec/live-debug.md`, built on the scripted server and the recipes of Live-debug steps 1 to 3. This section adds no check of its own.

