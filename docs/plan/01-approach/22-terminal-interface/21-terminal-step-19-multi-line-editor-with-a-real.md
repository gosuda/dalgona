#### Terminal step 19. Multi-line editor with a real cursor

The [shared input contract](../../../dal/spec/input-text.md) governs committed text, grapheme edits, and terminal IME ownership.
Keep UTF-8 bytes, rune positions, grapheme boundaries, and terminal cells distinct.
Paste, completion, and composition commits form atomic undoable edits. No input cleanup may rewrite accepted scalars.
Repair boundaries after adjacent joiner, combining-mark, Hangul, or regional-indicator changes.
Use real supported terminal/IME pairs for composition evidence. Synthetic text events cannot prove candidate-window behavior.

- `input.go`, renamed from `composer.go` by Step 1: rename the type `composer` to `editor`. The editor stays hand-written. `charm.land/bubbles/v2/textarea` has no chip or atom support (`bubbles/v2@v2.2.1/textarea/textarea.go`, no match for chip, atom or token), so it cannot show a paste chip as `[Paste #n +L lines]` or delete a chip as one unit (steps 28, 30, 31).
  - `shift+enter` and `ctrl+j` (LF decodes to `ctrl+j`, `ultraviolet decoder.go:1130-1171`) insert a newline character. `enter` when the char before the cursor is `\` replaces it with a newline character. `alt+enter` is not a newline key: it queues a follow-up (`internal/tui/model.go:200-201`).
  - `up` and `down` move between wrapped rows. `alt+left` and `alt+right` move by word. `ctrl+w` deletes a word back. `ctrl+k` kills to the end of the line. `ctrl+u` kills to the start of the line (today it kills to the start of the text, `composer.go:80-82`). `home` and `end` act per line.
  - Render `› ` on the first wrapped line in the thread hue as text colour (Design record, The point and the thread hue), and two spaces on the other lines. Delete the glyph cursor `▏` (`cursorView`, `composer.go:56-61`).
- Wrap and caret: `func (e *editor) layout(width int) (lines []string, row, col int)` wraps the text with `wrap` at `terminalWidth(width)`, and returns every wrapped line, the caret's wrapped row, and the caret's column in cells. `dockLines` (Step 3) gives the editor `c` rows, and shows the window `lines[start : start+c]` with `start = max(0, row - c + 1)`. So the caret stays visible and the editor scrolls inside itself beyond the cap. The editor keeps no scroll position. `placeholderView` shows on the input line while the text is empty.
- Cursor: `View` (`update.go`, today `model.go:637-642`) sets `view.Cursor = tea.NewCursor(x, y)` with `x = 2 + col` and `y = row - start`. It then sets `view.Cursor.Blink = m.screen.motion` (`bubbletea/v2@v2.1.1/tea.go:380-402`). `View.Cursor` is never nil while the client is inline, and the client draws no cursor glyph.
- Growth: the editor's wrapped line count changes only on a key press or a paste. A resize can change the wrap (Step 3).
- The placeholder becomes `Send a prompt · / commands · ? keys`.
- Symbols that change: `composer` becomes `editor`, and the file rename to `input.go` is Step 1. `cursorView` is deleted. `textView` is replaced by `layout`. `updateKey` changes `ctrl+u`. `View` sets `Cursor`.
- Tests (fail before): `TestEditorNewlineKeys` (shift+enter, ctrl+j and enter after a backslash each insert one newline character). `TestEditorCursorPosition` (text `ab` with the caret after `a`. Expected: `View().Cursor` is not nil, `X` is 3 and `Y` is 0. Source: this step and Bubble Tea practice rule 11). `TestEditorWrapsAtWidthMinusOne` (40 columns, 100 times `a`. Expected: every editor row is at most 39 cells). `TestEditorScrollsInsideBeyondCap` (ten lines typed at 40x8. Expected: at most 6 editor rows are drawn, `Y` lies inside them, and `len(dockLines())` is the same after the sixth line as at the sixth line. Source: invariant I2 of the section "Terminal screen design"). `TestComposerPlaceholderFollowsCursor` (update, `advisor_test.go:118`).
- Spec: `docs/spec/tui.md`, new section `Editor` (keys). Remove the glyph rule.
- Lands after: Steps 1, 2, 3 and 9.
- Anchor: Dieter Rams

