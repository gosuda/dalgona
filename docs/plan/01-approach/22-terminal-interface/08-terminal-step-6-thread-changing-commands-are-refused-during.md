#### Terminal step 6. Thread-changing commands are refused during a turn

- `slash.go`: add `func (m *model) busyRefusal(name string) bool`. When `m.thread.state` is `running`, `awaiting approval` or `awaiting input`, it calls `noticef("/%s is unavailable while a turn is %s; wait for it to finish or interrupt it first", name, word)` and returns true. `word` is `working`, `waiting for approval` or `waiting for an answer`. Call it at the start of `new`, `resume`, `fork`, `compact`, `tree`, `revert` and `review` (the last is added by Step 23) in `executeThreadCommand` (`commands.go:212`) and in `resumeCommand` (`commands.go:261`). The refusal is one notice row (step 2). A refused command makes no RPC call.
- Test (fails before): `TestThreadCommandsRefusedMidTurn`. Input: a table over the six commands with `m.thread.state` set to `running`. Expected: no RPC call is recorded, and the notice contains `unavailable while a turn is working`. A second case with `awaiting approval` expects `waiting for approval`, and a third with `awaiting input` expects `waiting for an answer`. The expected text comes from this step.
- Spec: `docs/spec/tui.md` keys table, a note on the six rows.
- Lands after: Steps 1 and 2.
- Anchor: Edsger W. Dijkstra.

