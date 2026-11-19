#### Terminal step 33. Rewind gesture

- `update.go`: Esc on an empty prompt, while idle, with no panel open and not in shell mode. When `hint.pending` is `rewind`, the rewind list opens. The list is `panelList` with one row for each turn of `dalgona/thread/tree` whose `active` is true, newest first. The row's `id` is the `turnId` and its label is the `preview` (`internal/rpc/types.go:174-182`, `TreeTurn`). Otherwise `setHint("Esc again to rewind", "rewind")` runs, and the hint shows in the live row. The live row changes no height.
- `enter` on the rewind list opens the confirm panel of Step 4 (`panelConfirm`) titled `Rewind to before this turn?`. Body line 1: `The prompt returns to the editor. Files changed by later turns are not reverted.` Actions: `[enter] rewind`, which is the point (Design record, The point and the thread hue), and `[esc] keep`.
- `enter` on the confirm calls `thread/revert` with `rpc.ThreadRevertParams{ThreadID, BeforeTurnID}`, where `BeforeTurnID` is the row's `id`. On success the prompt draft receives the row's label, the turn's first prompt cut to 80 characters by the server (`internal/agent/event/branch.go:91`). When the label holds 80 characters, the notice `Draft holds the first 80 characters of that prompt` shows.
- Symbols that change: `hintState.pending` takes the value `rewind` (Step 11). `rewindCommand` (new, `pickers.go`) builds the list from `dalgona/thread/tree`.
- Failing-first tests:
  - `TestEscEscRewindRestoresPrompt` (from the workstream). Input: two `esc` presses on an empty idle prompt, then `enter` on the list and `enter` on the confirm. Expected: the prompt holds the row's label, the turn's first prompt.
  - `TestRewindHintInLiveRow` (new). Input: one `esc` on an empty idle prompt. Expected: the live row reads `Esc again to rewind`, and the frame height is unchanged. Source: cross-phase decision 52.
  - `TestShellModeEscLeavesWithoutRewind` (moved here from Step 30). Input: `!`, `esc`, then `esc` on the empty prompt. Expected: the mode is `inputPrompt` after the first `esc`. The second `esc` sets the hint `Esc again to rewind` and opens no list. Source: this step.
- Spec: adds one row to the keys table of `docs/spec/tui.md`: `esc` on an empty prompt opens the rewind list or sets the hint. This step adds no other spec text.
- Reference: omp2 `crates/chat/src/composer.rs:304-311` and grok-build `views/rewind.rs:39-83` (via the feature reports). Taken: double Esc on empty; the phases picker, confirm, execute; `files stay`. Left: tree target choice.
- Lands after: Steps 3, 4, 11, 13, 21 and 30.
- Anchor: J.R.R. Tolkien

