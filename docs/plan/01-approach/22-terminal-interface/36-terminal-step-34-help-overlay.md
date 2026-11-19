#### Terminal step 34. Help overlay

- `help.go`: `type keyHelp struct{ group, keys, action string }` and `var keyTable = []keyHelp{...}` with groups `Compose`, `Turn`, `Panels` and `Review`. One row for each key that the steps bind (`alt+enter` queues a follow-up, and a newline is `shift+enter`, `ctrl+j`, or a backslash before `enter`):

| Key | Group | Action | Step |
|---|---|---|---|
| `enter` | Compose | sends the prompt, runs a shell line, or activates the highlighted panel row | 19, 21, 30 |
| `shift+enter`, `ctrl+j` | Compose | inserts a newline | 19 |
| `\` then `enter` | Compose | replaces the backslash with a newline | 19 |
| `alt+enter` | Compose | queues the draft as a follow-up (`internal/tui/model.go:200-201`) | 32 |
| `up`, `down` | Compose | move between wrapped rows; on the first or last line, recall history | 19, 20 |
| `alt+up` | Compose | moves the newest queued text back into an empty editor | 32 |
| `alt+left`, `alt+right` | Compose | move by word | 19 |
| `ctrl+w`, `ctrl+k`, `ctrl+u` | Compose | delete a word back, kill to line end, kill to line start | 19 |
| `home`, `end` | Compose | move to the line start or end | 19 |
| `tab` | Compose | completes the slash command or `@file` | 22, 29 |
| `backspace`, `delete` | Compose | remove a whole paste or shell chip | 28, 30 |
| `ctrl+v` | Compose | pastes an image from the clipboard | 31 |
| `!` on an empty prompt | Compose | shell mode; `esc` leaves it | 30 |
| `/`, `//` | Compose | opens the slash palette; `//text` sends text that starts with `/` | 7, 22 |
| `@` | Compose | opens file completion | 29 |
| `?` on an empty prompt | Compose | opens the help panel | 34 |
| `esc` while working or waiting | Turn | first press shows `Esc again to interrupt`; second interrupts; one press in a rate-limit wait interrupts | 11, 13 |
| `esc` on an empty idle prompt | Turn | first press shows `Esc again to rewind`; second opens the rewind list | 33 |
| `ctrl+c` | Turn | clears a draft into history; otherwise first press shows `Ctrl+C again to exit`, second quits | 13, 20 |
| `ctrl+z` | Turn | suspends the client | 37 |
| `ctrl+o` | Turn | opens the full-screen review | 15 |
| `alt+q` | Turn | opens the queued follow-ups panel | 32 |
| `up`, `ctrl+p`, `down`, `ctrl+n` | Panels | move the list cursor | 21 |
| `pgup`, `pgdown` | Panels | move by a page | 21, 34 |
| `enter` | Panels | activates the highlighted row or confirms | 4, 21, 23, 24, 25, 33 |
| `esc` | Panels | clears the filter, then closes the list; cancels a confirm; closes the help panel | 4, 21, 25, 26, 34 |
| `tab`, `shift+tab` | Panels | cycle the efforts of the highlighted model | 21 |
| `ctrl+r` | Panels | opens the advisor reset confirm | 23 |
| `y`, `a`, `n` | Panels | accept, accept for session, decline (approval card) | 26 |
| `up`, `down` | Panels | scroll the approval card, settings rows and help lines; move the popup highlight | 3, 22, 24, 26, 34 |
| `left`, `right`, `space` | Panels | settings: cycle a value, move between status items, toggle | 24 |
| `up`, `k`, `down`, `j` | Review | scroll one line | 15 |
| `pgup`, `pgdown` | Review | scroll a page | 15 |
| `home`, `g`, `end`, `G` | Review | go to the top or bottom | 15 |
| `[`, `]` | Review | previous or next entry | 15 |
| `esc`, `q`, `ctrl+o` | Review | close the review | 15 | Keep `keyTable` hand-written, not `help.Model`. `help.Model.View(k KeyMap)` renders a `help.KeyMap` of `key.Binding` values in columns (`bubbles/v2@v2.2.1/help/help.go:108-110`). It cannot draw group headers, the Status legend or the Step 3 panel frame.
- Kind `panelHelp`: 14 rows at 24 rows or more. It holds a title, 12 body rows and a hint row. The body is the key table, then the `Status` group, scrolled by line. It holds no text field, so printable keys do nothing, except `?`, which closes it.
- `up` and `down` scroll one line. `pgup` and `pgdown` scroll 12 lines. `esc` and `?` close the panel.
- `?` on an empty prompt opens the panel. `/help` (added to `commands` with the summary `Show keys`) opens it too.
- Status group: added when the panel opens. It has one line per shown status item (Design record, Status band ids; hidden items get no line), in the form `<icon> <id> <value>`, or `<id> <value>` when the item has no icon. Examples use the glyphs of table T2 and the context rule of cross-phase decision 18: `⣶ effort high`, `✓ approval auto`, `context 25%`, `model gpt-x`, `mark dalgona`. 
- The placeholder is set in Step 19.
- Symbols that change: `keyTable` and `keyHelp` (new, `help.go`). The help panel uses `panelView.scroll` and no other state.
- Failing-first tests:
  - `TestHelpOverlayListsKeysAndCloses` (from the workstream). Input: `?`, then `esc`. Expected: the table lines appear, and the panel closes.
  - `TestHelpStatusLegendPerShownItem` (from the workstream). Input: `status_hide` `["folder"]`. Expected: seven status lines for a fixture where every other item shows, and no line contains `folder`.
  - `TestHelpScrollReachesLastLine` (new). Input: 50 `down` presses. Expected: the last line of the Status group is in the frame. Source: the body length in `keyTable` plus the legend.
  - `TestHelpDockSteady` (new). Input: `assertSteadyDock` with 50 `down` presses. Expected: 17 frame rows (3 plus 14). Source: Step 21 table.
- Spec: `docs/spec/tui.md` keys table (`?`, `/help`).
- Reference: grok-build `views/shortcuts_help.rs:1-63` (via agent://GrokBuildFeatures/report). Taken: grouped modal. Left: search.
- Lands after: Steps 3, 7, 12, 13, 15, 19, 20, 21, 22, 30, 32 and 33.
- Anchor: Saul Bass

