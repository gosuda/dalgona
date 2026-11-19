#### Terminal step 4. `/revert` asks for confirmation

- `panels.go`: use a `panelView` (Step 3) with `kind` set to the action (`revert`, `new`, `compact` or `rewind`), `id` set to the target where there is one, and `decision` true. No other type is added. Step 4 sets `layout` to `panelConfirm` (Step 3), so the rows are `panelRows(panelConfirm, screen.height)`: 6 rows, capped at the window height minus 4.
- Body: four rows. Row 1 is `<preview>` in backticks, with line breaks replaced by spaces and cut to one row with `m.screen.fit`. The text `Later turns leave the active path. Files changed by later turns are not reverted.` wraps with `m.screen.wrap` into the rows after it. Rows past the four are cut and end with `...`.
- Title: `Revert to before this turn?`.
- Actions on one line: `[enter] revert  [esc] keep`. `[enter] revert` is the point (Design record, The point and the thread hue).
- Keys: `enter` calls `thread/revert` with `rpc.ThreadRevertParams{ThreadID, BeforeTurnID}`. `esc` closes the panel. The panel holds no text, so the input line keeps the prompt draft.
- Symbols that change: `activateChoice` (`commands.go:302-343`, moved to `pickers.go` by Step 1) opens the confirm panel for kind `revert`. It no longer calls `thread/revert` directly. No new symbol.
- Edge cases: an empty preview shows an empty pair of quotes. A preview longer than one row is cut with `...`.
- Failing-first tests:
  - `TestRevertNeedsConfirmation` (from the workstream, unchanged). Input: Enter on the revert picker, then Enter again. Expected: the first Enter makes no `thread/revert` call and shows `Revert to before this turn?`. The second Enter makes the call. Source: this step and product decision 13.
  - `TestRevertPreviewIsOneRow` (new). Input: a preview with a line break between `a` and `b`, width 40. Expected: the preview row holds `a b`, and no frame row starts with `b`. Source: practice rule 4 and invariant I6.
  - `TestConfirmPanelSteady` (new). Input: a revert confirm open at 24 rows with a one-line draft; `assertSteadyDock` with 50 `down` presses. Expected: 9 frame rows before and after each key (3 plus 6). Source: the `panelConfirm` row of the Step 3 table and invariant I2.
- Spec: `docs/spec/tui.md` keys table, `/revert` row.
- Lands after: Steps 1, 2 and 3.
- Anchor: Tony Hoare

