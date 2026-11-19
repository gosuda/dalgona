#### Terminal step 20. Prompt history

- `history.go`: `type historyEntry struct{ turnID, text string }` and `type history struct{ entries []historyEntry; index int; draft string }`, cap 200, consecutive duplicates dropped. `turnID` is the turn that a sent prompt started, and it is empty until that turn starts. A text whose first token is `/login` is never added. Sources: `applyResumed` user messages and `item/completed` userMessage items.
- `up` on the first line recalls the previous entry and saves the draft. `down` on the last line moves forward and restores the draft past the newest entry. On the other lines, `up` and `down` move between visual lines (step 19).
- A recalled text can make the composer longer. The editor shows at most `composerMaxRows` (6) rows (Step 3). The dock height changes only on this key press (invariant I2 of the section "Terminal screen design"), and its rows stay inside the window (invariant I6).
- Ctrl+C with a non-empty draft clears it into history and calls `setHint("Draft saved to history · Ctrl+C again to exit", "exit")`. The hint uses the live row, cut to one row with `fit`, as the design requires. Step 13 owns the two-press window.
- Tests (fail before):
  - `TestHistoryRecallAndDraftRestore`: send `a`, type `dr`, `up` recalls `a`, `down` restores `dr`. Expected: the editor holds `a`, then `dr`.
  - `TestLoginLineNotInHistory`: send `/login openai`. Expected: `history.entries` is empty.
  - `TestCtrlCSavesDraft`: type `dr`, then Ctrl+C. Expected: the editor is empty, `history.entries` holds `dr`, and the hint reads `Draft saved to history · Ctrl+C again to exit`.
- Test (fails before, `TestHistoryRecallKeepsFrameInWindow`): at 40 by 8, recall a 20-line draft. Expected: `len(lines) <= 8`, and each line has `cells <= 39`. Source: invariant I6 of the section "Terminal screen design".
- Spec: `docs/spec/tui.md`, its history keys added to Editor (Step 19 owns the section).
- Reference: omp2 `crates/chat/src/history.rs:91-145` (via agent://Omp2Features/report). Taken: secret-bearing `/login` lines are skipped. Left: SQLite store, Ctrl+R search.
- Lands after: Steps 3, 11, 13 and 19.
- Anchor: Milton Erickson.

