#### Terminal step 21. List panels and the model picker with presets

The rules in this step bind Steps 4, 22 to 27, 30 and 33 to 35.

Shared panel rules:
- Kinds. Defined in Step 3 (`panelKind`). The kind sets the height only (cross-phase decision 53). The queue panel is a `panelList`. `panelView.kind` (Step 3) is the action name, and `panelView.layout` is the panel kind.
- Height. `panelRows` (Step 3) returns the row count of the kind in the table below, capped at the window height minus 4 (cross-phase decision 53). The three rows that are not panel rows are the input line, the live row and the status band.

| Kind | Rows | Contents |
|---|---|---|
| panelPopup | 8 | slash palette and `@file` completion |
| panelList | 12 | list panels: model, resume, tree, fork, advisor, review, rewind and queue |
| panelConfirm | 6 | confirm panels: revert, new, compact and rewind |
| panelApproval | 9 | approval card |
| panelAsk | 13 | question card (the ask workstream, Step 7) |
| panelSettings | 17 | `/settings` |
| panelLogin | 12 | `/login` |
| panelHelp | 14 | key list |

A panel that is cut keeps its title row and its action rows, and loses body rows first (cross-phase decision 53).

- Dock height. `len(dockLines())` is the composer rows, plus `panelRows`, plus 1 for the live row, plus 1 for the status band. While a panel is open, the composer shows one row: the input line. With no panel open, the composer shows at most `composerMaxRows` (6) rows. The other draft rows keep their text and show again when the panel closes.
- Opening and closing. Opening a panel changes the height once, from the composer's rows to 3 plus `panelRows`. Closing changes it back once. Typing, moves, ticks, streamed text and notices never change it (invariant I2 of the section "Terminal screen design"). A window resize may change the dock height (cross-phase decision 52).
- Cursor. A list, confirm, decision, settings, login or help panel moves the draft cursor to the end of the first line of the draft, and the input line shows that line. The slash and file popups do not move the cursor, so the token being typed stays under it. The cursor returns to its old place when the panel closes. `View.Cursor` sits on the composer row that holds the insertion point (Bubble Tea practice rule 11).
- Frame. `framePanel` (Step 3) draws the rows and pads them with blank rows to the fixed count. Panel rows use box-drawing characters, arrows and dots, because no panel row has a right-anchored field. Each row is measured with `cells` and fitted to `screen.width - 1` (Bubble Tea practice rule 4).
- Stacking. A decision (Steps 26 and 27) opens above a user panel and closes back to it. A list panel loses its filter when a panel opens above it, so the input field is cleared.

Input line:
- `input.go`: `type textField struct{ text []rune; cursor int }` holds the text of the input line in text modes. It serves the rule that no panel holds text.
- `input.go`: `m.input.mode` is `inputPrompt` or `inputShell` and is stored. The other text modes come from the top panel through `func (m *model) textMode() inputMode`. It returns `inputTarget`, `inputFilter`, `inputAnswer` or `inputAPIKey` from the top panel's state (`inputAnswer` is set by the question card, the ask workstream, Step 7), and `inputPrompt` or `inputShell` otherwise.
- One row. When the text is wider than the row, the field shows the cells around the cursor and marks each cut with `...`. An input line never wraps.
- Printable keys edit the field in a text mode only. In a card without a text mode, printable keys do nothing, so `y` and `a` keep their card meaning.
- A multiline paste into a one-line text field fails atomically. Preserve its previous value and selection and keep the candidate recoverable. Steps 28 and 31 cover the prompt.
- A notice or a hint shows in the live row and changes no height (design, Structure of the dock).

| Mode | Label | Typed | Enter | Esc |
|---|---|---|---|---|
| inputPrompt | `› ` in the text colour of the thread hue | the prompt draft | sends the prompt (`submitPrompt`) | interrupts while working (Step 13), starts the rewind gesture when empty and idle (Step 33) |
| inputShell | `! ` in the text colour; the placeholder reads `Shell · enter runs · esc leaves` | a shell command (Step 30) | runs it (Step 30) | leaves shell mode (Step 30) |
| inputFilter | `filter: ` | filters the list | runs the highlighted row | clears the filter; with no filter, closes the panel |
| inputTarget | `branch: `, `commit: ` or `instructions: `, chosen by `listPanel.pending` | the review target (Step 23) | sends the review | clears the text and `pending`; the list stays open |
| inputAnswer | `answer: ` | the Other text or a free-text answer (the ask workstream, Step 7) | keeps the text and moves on (the ask workstream, Step 7) | back to the option rows, or dismisses a free-text question (the ask workstream, Step 7) |
| inputAPIKey | `API key: `, one `*` per rune | the key (Step 25) | sends `account/login/start` (Step 25) | clears the key; back to the method rows (Step 25) |

List panel:
- `panels.go`: `type listPanel struct{ kind, title string; items []listItem; cursor, scroll int; pending string; err string }`. The `filter` field is removed, because the filter is the field text. `pending` names the row that waits for typed text (Step 23). `listItem` is unchanged.
- `func fuzzyScore(query, text string) int` is new, in `panels.go`. It ranks the rows of a filter: 3 when `text` starts with `query` (case-insensitive), 2 when `text` contains `query`, 1 when the runes of `query` occur in `text` in order, and 0 otherwise. An empty `query` gives 3.
- Visible item rows: `panelRows(panelList, h) - 2`, for the title and the hint. `err`, when set, replaces the hint row.
- The list stays hand-written: `list.Model` owns its cursor, scroll and chrome (`bubbles/v2@v2.2.1/list/list.go:148-152` show flags, `:185` `FilterInput`, `:216-217` filter prompt), and `list.View` (`list.go:1042`) draws the whole component, so the rail and action block of Step 3 would sit over chrome the list chose. Cross-phase decision 23 allows a hand-written widget when no component meets the step's requirements, and this one does not.
- Keys: `up` and `ctrl+p` move up; `down` and `ctrl+n` move down; `pgup` and `pgdown` move by a page (the visible rows). Printable keys, `backspace`, `delete`, `left` and `right` edit the field. `enter` activates the highlighted row. `esc` clears the filter first, and closes the panel when the filter is empty. `tab` and `shift+tab` cycle the efforts of the highlighted model. They replace `left` and `right` for effort, because those keys move the field cursor.
- An empty list shows blank rows, and `enter` does nothing.
- Model picker (`pickers.go`): the `Presets` section comes from `model/list` items whose `presets` is non-empty (dependency P6), shown as `default · <id>`, `smol · <id>`, `slow · <id>`, then all non-hidden models. `tab` and `shift+tab` cycle the row's `supportedReasoningEfforts`. `enter` calls `config/value/write` with key `model`, then with key `model_reasoning_effort` when the effort changed (the protocol workstream, Step 10). A failure keeps the panel open with `Could not save: <err>` in the hint row.
- The resume, tree and revert panels move to `listPanel`. Delete `selector`, `choice`, `renderSelector` and `selectKey` (`model.go:722-741`, `commands.go:302-343`). `listItem` replaces `choice`. Labels, details and badges pass through `sanitizeText` (Step 2).

Shared test helper:
- `assertSteadyDock` is defined in Step 3.

Symbols that change: `listPanel` and `listItem` (`panels.go`), `textField`, `inputMode` and `textMode` (`input.go`), `applyModels`, `applyThreads` and `applyTree` (`pickers.go`).

Failing-first tests:
- `TestModelPickerFilterAndEffort`. Input: filter `gpt`, then `tab`, then `enter`. Expected: two writes, `model` then `model_reasoning_effort`. Source: the key rule of this step (`tab` replaces `right`).
- `TestModelPickerShowsPresets` (from the workstream, unchanged).
- `TestResumePickerFilters`. Input: filter `demo`. Expected: only rows with a `fuzzyScore` above 0 for `demo` remain. Source: the tiers of `fuzzyScore` in this step.
- `TestPickerEscClearsFilterThenCloses` (from the workstream, unchanged).
- `TestPickerSaveErrorStaysOpen` (from the workstream). Expected: the panel stays open and the hint row reads `Could not save: <err>`.
- `TestListDockSteady`. Input: `assertSteadyDock` with 50 printable keys, then 50 `down` presses. Expected: 15 frame rows throughout (3 plus 12). Source: the table above and invariant I2.
- `TestListOpenAndCloseChangeHeightOnce`. Input: open the resume list, then close it. Expected: 3, then 15, then 3. Source: the height rule above.

Spec: `docs/spec/tui.md`, new section `Panels`: the height table, the input line modes table, the list keys and the model picker.
- Lands after: Steps 1, 2, 3, 11, 12, 13 and 19.

- Anchor: Herbert Simon

