#### Terminal step 29. `@file` completion

- `complete.go`: the file popup is kind `panelPopup` (8 rows, Step 21). It opens when `@` starts a token of the prompt draft and no panel is open. Its query is the text after `@` in that token.
- After 120 ms without typing, `debounceMsg{seq}` (one `tea.Tick`) starts `fuzzyFileSearch {query, roots: [opts.Cwd]}` (dependency P1) in a `tea.Cmd`. Only the result for the newest `seq` is taken (`searchResultMsg{seq, files, err}`). Older results are dropped.
- Show at most 8 rows. Each path passes through `sanitizeText` first, because a file name can hold control bytes. `tab` or `enter` inserts `@<path> ` in place of the token. The server returns files only (dependency P1).
- Error -32601 shows `File search is unavailable on this server` in the first popup row, on the first such error of the session. Later errors leave the popup empty.
- Symbols that change: `completion` (Step 22) gains the file kind. `debounceMsg` and `searchResultMsg` are the Structure messages.
- Failing-first tests:
  - `TestAtFileCompletionInsertsPath` (from the workstream). Input: type `@modl`, server result `pkg/model.go`, then `enter`. Expected: the draft holds `@pkg/model.go `.
  - `TestFileSearchStaleResultIgnored` (new). Input: the result for `seq` 1 arrives after the result for `seq` 2. Expected: the popup shows the rows of `seq` 2 only. Source: the `seq` rule above.
  - `TestFilePopupSteady` (new). Input: `@`, then 50 letters, then 50 `down` presses through `assertSteadyDock`. Expected: 11 frame rows (3 plus 8). Source: Step 21 table.
- Spec: one paragraph (`@` completion) in the `Editor` section that Step 19 creates. This step adds no other spec text.
- Reference: omp2 `crates/chat/src/autocomplete/files.rs:28-65` (via agent://Omp2Features/report). Taken: directories end in `/`. Left: emoji and GitHub refs.
- Lands after: Steps 3, 19, 21 and 22.
- Anchor: Herbert Simon

