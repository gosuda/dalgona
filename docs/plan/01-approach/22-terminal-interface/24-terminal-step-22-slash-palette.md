#### Terminal step 22. Slash palette

- `complete.go`: `type completion struct{ kind string; items []listItem; cursor int }`. The popup is kind `panelPopup` (8 rows, Step 21). It has no text field: its query is the first token of the prompt draft.
- The popup opens when the first line starts with `/` (not `//`), the cursor is in the first token, and no panel is open. Opening a panel closes the popup.
- Rows come from `commands` (Step 7), ranked by `fuzzyScore`, at most 8, shown as `/name args  summary`. Rows below the items are blank.
- `tab` completes `/name `. `enter` runs the highlighted command when its `args` is empty. Otherwise it completes `/name ` and closes the popup, so the argument is typed in the editor. `esc` closes the popup and keeps the draft. Typing a character reopens it. `up` and `down` move the highlight. Printable keys edit the draft.
- The popup closes when the cursor leaves the first token.
- Symbols that change: `completion` (new, `complete.go`). The popup reads `commands` and `fuzzyScore`.
- Failing-first tests:
  - `TestSlashPaletteFiltersAndCompletes` (from the workstream, unchanged). Input: `/mo`. Expected: the palette shows `/model`, and `tab` gives `/model `.
  - `TestPopupSteady` (new). Input: type `/`, then 50 letters, then 50 `down` presses through `assertSteadyDock`. Expected: 11 frame rows throughout (3 plus 8). Source: Step 21 table and invariant I2.
- Spec: `docs/spec/tui.md`, one palette paragraph added to Editor (Step 19 owns the section).
- Reference: omp2 `crates/chat/src/autocomplete/slash.rs:97-151` (via agent://Omp2Features/report). Taken: popup below the prompt; Tab accepts; Esc keeps the draft. Left: skills grouping, MRU.
- Lands after: Steps 7, 19 and 21.
- Anchor: Claude Shannon

