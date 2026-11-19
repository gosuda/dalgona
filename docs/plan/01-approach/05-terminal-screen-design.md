### Terminal screen design

The owner's requirement: nothing on the screen shakes, and no screen is drawn again. This section derives the terminal screen from what a terminal is. Its invariants I1 to I8 bind every step of the terminal workstream.

#### Primitives

These are the facts the design cannot exist without. None follows from another.

1. Grid. The terminal shows rows and columns of cells. The program can only append bytes to its output. It cannot read the screen.
2. Scrollback. A row that leaves the top of the grid belongs to the terminal. The program cannot change it.
3. Terminal-owned layout. The window changes size at any time. The terminal then lays out what it shows again by its own rules. The terminal also decides how wide each character is.
4. No atomic write. The terminal shows bytes as they arrive, unless it supports synchronized output.
5. Position is identity. A reader follows content by its place. Content that did not change and still moves, or that goes away and comes back, reads as shaking or flicker. This is the requirement itself.
6. Finished items do not change. The journal is append-only.
7. Time passes. Clocks, counters and streamed text change with no key press.
8. Keys are events. A key press has one expected effect, at the cursor, at once.

#### Derived structure

Each element names the primitives that force it.

##### Two regions (1, 2, 6)

The output has two regions. History is everything above the dock. The dock is the last rows, and it is the only part that changes. A finished item is written to history one time, as rows above the dock. No row of history is ever written again: not on resize, not on a theme or verbosity change, not when a thread is loaded, and not on exit.

##### Never clear (2, 5)

A clear removes the identity of every row at once, and it cannot bring scrollback back. While the client is inline it never erases the display, never erases the scrollback, never resets the terminal, and never moves the cursor to the home position.

##### The dock keeps its height (5, 7)

A change of the dock's height moves the dock's own rows or moves history. Only an event that needs the user's attention may do that. The height is a function of three things: the number of lines in the composer, which decision panel is open, and the window height, which caps the dock (print rule 4). Time, streamed content, progress, the start and the end of a turn, notices and hints never change it.

- The live row is there in every window of 3 rows or more. When no turn runs, it is empty, and a notice or a hint uses it.
- A panel has one height for its kind, capped by the window height. A list inside a panel has a fixed number of rows. Typing in a filter never changes the height.
- Progress that the agent reports (a plan, subagents, queued work) is a fixed field in the live row and a history row when it changes. It is never a region that grows.
- The composer grows only when the user adds a line, up to a fixed limit. Beyond the limit it scrolls inside itself.
- The dock is never taller than the window minus one row, so a print always has room.

##### Fixed fields (5, 7)

Inside a row, a value that changes with time keeps its cells. The elapsed time, the context percent, the queue count and the live dot each have a fixed width and a fixed place. Fields at the right are anchored to the right edge. Free text is cut with an ellipsis and never pushes a field, and in the live row it follows the fixed fields.

##### A stream is written in final blocks (2, 6, 7)

Partial content cannot go to history, because it may still change. So a streamed item is cut where its rendering is final, and each block is written once at that moment. The part that is not final shows only as a tail in the live row, on one row.

- Answer text: a block is final at a blank line outside a code fence, a list, a table and a quote. A heading line and a rule line are final at their line end.
- Fenced code: each line is final when its line ends. The fence opens with one header row.
- Reasoning: the tail shows in the live row. At the end the client writes one summary row, or the whole text at verbosity `high`.
- A tool call: the live row shows its name and the tail of its output. When it ends, the client writes its row, and the last lines that the verbosity level keeps.
- Calls that run at the same time: each writes its row when it ends. The live row shows how many run.

##### One paint (4)

A change of the dock is one write, inside synchronized output where the terminal supports it. The client never erases a row and then draws it. Only cells that changed are written. A history row is written by its own print, and a later frame draws the dock again (print rules 2 and 3). Bubble Tea gives no order between a print and a frame flush (upstream issue 1736).

##### Widths the terminal agrees with (3)

A dock row that wraps changes the dock's height, so no dock row may wrap.

- A fixed field uses only glyphs of East Asian Width `N`, `Na` or `H`, so it has the same cells in every terminal setting. A Nerd Font glyph is followed by one space.
- All text is measured with the method that the renderer uses (`ansi.WcWidth`, or `ansi.GraphemeWidth` after the terminal reports mode 2027). The client and the renderer then agree on every cell, and the cursor is where the text is. Text is cut by that measure.
- Every dock row is at most one cell narrower than the window.

##### Resize (3, 2)

The terminal lays out history again by itself. The client redraws only the dock. After the window narrows, the terminal may have rewrapped the rows of the old dock, and the client cannot see that. The only place the client still knows is the cursor, because the terminal keeps the cursor on its line. So the old dock must be removable from the cursor alone: the cursor sits on the composer row that holds the insertion point. The composer is the first block of the dock, so every row above that cursor row is composer text, and every row below it is the rest of the dock.

##### One input line (8, 3)

The cursor marks the one place that takes text: the insertion point in the composer. The composer is the first block of the dock and may hold several rows, and the cursor sits on the row that holds the insertion point. All text is typed there: a prompt, a filter for a list, a reason, another answer, a key. A panel below it shows choices and never holds a second text field. The label at the start of the composer says what is being typed.

##### One clock (7)

One tick chain drives all motion. A tick changes only fixed fields. The chain stops when nothing moves.

##### Full screen only by key (5, 8)

The alternate screen opens only when the user presses the review key, and closes only by key. Rows that finish while it is open are held and written to history when it closes. The client repaints nothing of the normal screen on return, because the terminal restores it.

##### Keys never wait (8)

A key press changes the dock in the next frame. No key effect waits for I/O.

##### Order on the screen is the order in time (2, 6)

The screen is a log. Start prints a welcome row. Loading another thread prints a thread row and then that thread's rows. Exit removes the dock and prints one exit row. Nothing is removed to make room.

##### Colours that cannot be taken back (2)

A written row keeps its colours. Rows written before the theme is known use only the terminal's default foreground, bold and dim.

#### Invariants and their checks

Formal notation uses ASCII. `H(t)` is the sequence of rows above the dock after message `t`. `height(t)` is the number of rows of the dock.

| Id | Invariant | Check |
|---|---|---|
| I1 | `H(t)` is a prefix of `H(t+1)` while the window size is the same | Feed the output of a scripted session to a terminal emulator and compare the rows above the dock after each message |
| I2 | `height(t+1) != height(t)` only when message `t+1` edits the composer, opens or closes a panel, or resizes the window | Count the lines of `View().Content` over a scripted turn with streaming, ticks, notices and tool output |
| I3 | Each time-driven field has the same column range in frame `t` and frame `t+1` while the width is the same | Find each field's columns in two frames that differ only in the field's value |
| I4 | The output holds no erase-display, erase-scrollback, reset or home sequence while the alternate screen is off | Search the captured bytes of a scripted session |
| I5 | Each finished item is printed exactly once | Count the print commands for each item key |
| I6 | `height(t) <= max(0, window_height - 1)` and `width(row) <= window_width - 1` for every dock row | Assert on every frame of the scripted session at two window sizes |
| I7 | After a narrowing, no history row is written again, and no row of the old dock is left above the new dock except the typed text of the composer (see Resize mechanics) | The live session narrows the window in the middle of a turn in a real terminal (scenario T42); `TestInvI7NarrowingPrintsNothing` checks that no row prints while the narrowed frame settles |
| I8 | A tick with no change of state writes nothing outside the fixed fields | Compare the bytes written between two frames that differ by one tick |

#### Open assumptions

- The live row sits below the composer. The resize rule forces it: a row above the cursor's row can leave a stale copy on screen after a narrowing.

- The width rule assumes the terminal draws an East Asian Ambiguous character one cell wide. A user whose terminal draws it two cells wide sets `RUNEWIDTH_EASTASIAN=1`, which the renderer and the client both read.
- In a terminal that draws ambiguous characters wide without `RUNEWIDTH_EASTASIAN=1`, a row that holds such characters can be wider than measured. `docs/spec/tui.md` states this limit.
- The stable boundaries for answer text assume that a reference-style link whose definition comes later renders as written.

#### What the renderer does

These facts were read in the source of Bubble Tea v2.1.1 and its terminal renderer (`ultraviolet`, the version in `go.mod`). `BT` is `charm.land/bubbletea/v2@v2.1.1` and `UV` is `github.com/charmbracelet/ultraviolet@v0.0.0-20261008173134-6b8d4baf91b4` in the module cache.

| Fact | Source | What the design does with it |
|---|---|---|
| A change of the frame's height or width erases from the frame's first row to the end of the screen and paints the whole frame again. Without synchronized output this shows as a flash. | `BT/cursed_renderer.go:316-336`, `UV/terminal_renderer.go:1493-1495`, `:1273-1276` | The dock changes height only on a user edit of the composer or when a panel opens or closes. |
| A frame taller than the window loses its top rows, and its top rows are dropped on each paint. A view that did not change is not written again (`BT/cursed_renderer.go:326-328`). | `BT/cursed_renderer.go:353-357` | The dock is at most the window height minus one row. |
| An unchanged view writes nothing. A changed cell writes only its span. | `BT/cursed_renderer.go` flush, `UV/terminal_renderer.go` transformLine | A tick changes only the cells of fixed fields. |
| Several updates between two ticks give one paint. | `BT/tea.go` render and ticker | State may change freely between paints. |
| On resize the renderer moves up by the cursor's row inside the frame, erases to the end of the screen, and repaints. It cannot see rows that the terminal rewrapped. Rows of the old dock stay on screen when a row above the cursor's row wrapped, or when the cursor's column is not less than the new width. | `BT/cursed_renderer.go:682-695`, `UV/terminal_renderer.go` reconcileSize and clearUpdate; upstream issue 1867 reports the effect | The cursor sits on a composer row. The rows above it are composer rows, so a row that stays on screen is the user's own typed text. |
| With `View.Cursor` nil the cursor rests after the last written cell, and moves to column 0 of the next line when that cell is in the last column. | `BT/cursed_renderer.go:531-541` | `View.Cursor` is always set while the client is inline. |
| `tea.Println` writes at once and outside synchronized output: it moves to the frame's last row, scrolls with line feeds, moves up, inserts lines, and writes the text. It counts a line wider than the window as wrapped rows, and miscounts a line whose width is an exact multiple of the window width. When the frame height plus the printed rows exceed the window height, its position model is wrong. | `BT/cursed_renderer.go:771-830` | The client wraps every printed line itself to at most one cell less than the window width. One print holds at most the window height minus the dock height in rows, which is at least one row because the dock is at most the window height minus one (print rule 4). A longer block is printed in several prints, one in flight at a time. |
| `tea.Println` is not blocked while the alternate screen is on, because the event loop calls `insertAbove` with no mode check (`BT/tea.go:892-893`). `Program.Println` documents that it prints nothing there (`BT/tea.go:1419`). The plan relies on neither behaviour. | `BT/cursed_renderer.go:771-830` | Rows that finish while the review is open are held. They print after it closes and the renderer has left the alternate screen. |
| The renderer changes the frame height and the screen mode only at its next flush, 60 times a second. `insertAbove` uses the height of the last frame that it drew. | `BT/cursed_renderer.go:296-368`, `:772-826`, `BT/renderer.go:13` | A print starts only after the frame has settled for three frames (50 ms), and while a print is in flight `View()` returns the frozen view. |
| tmux 3.6 option `scroll-on-clear`, on by default. When the frame starts at the top-left cell of the pane, each height change of the frame copies the screen into the history, because the renderer erases with `CSI J` from the frame top. | Measured on 2026-11-12, 10 runs per case: an 8-row dock in an 8-row window left a copy of the dock in the history, a 7-row dock left none, and a program whose first view is empty until its first row prints left none. | The dock is at most the window height minus one row (print rule 4), and `View()` is empty until the first row has printed (print rule 5). |
| Upstream `charmbracelet/bubbletea` issues #1736 (no way to order `tea.Println` with the frame flush) and #1881 (a print returned from the update that leaves the alternate screen is lost) were open on 2026-11-12, and v2.1.1 is the latest release. | https://github.com/charmbracelet/bubbletea/issues/1736 and https://github.com/charmbracelet/bubbletea/issues/1881 | No API orders a print with a flush, so a print waits for the settled frame (print rule 2). |
| Synchronized output wraps frame updates only, and only when the terminal reports the mode. It is not asked for over SSH or in Apple Terminal. | `BT/tea.go:991-1002`, `:1141-1146`, `BT/cursed_renderer.go:591-617` | The design never needs atomic output to be correct. It only reduces the events that repaint the dock. |
| The frame is measured by code-point width until the terminal reports grapheme clusters. `tea.Println` counts by grapheme width. | `UV` buffer width method, `x/ansi@v0.11.9/width.go:66-68` | The client measures with the widest reading and keeps every row one cell short of the window width. |
| Entering the alternate screen clears only the alternate buffer. Leaving it restores the main screen, and the renderer repaints only the dock in its old place. | `BT/cursed_renderer.go:718-724` | The review may use the alternate screen. |
| On quit the renderer paints the last view, moves to the last row of the frame and erases from there. With an empty last view it moves one row up and erases the last history row. | `BT/cursed_renderer.go:201`, close path | The last view is one blank row. The exit row is printed before quit. |
| On suspend the renderer does the same as on quit, and after resume it treats the cursor's line as the first row of a new frame. | `BT/tea.go:586-600`, `BT/tty.go` | The view becomes one blank row in the update that starts the suspend. `finishSuspend` returns `tea.Suspend` only after the rows have printed (Terminal step 13). |

#### Layout of the dock

From top to bottom:

1. The composer, from its first row to its last. The cursor sits on the row that holds the insertion point, and the composer keeps that row in view when it scrolls.
2. The open panel, when there is one. Its height is fixed for its kind.
3. The live row. It is always there.
4. The status band.

The input line is never hidden. When the window is too short for a panel, the panel has fewer rows and scrolls inside itself.

The live row reads from the left: the state word in a field of 8 cells, the elapsed time in a field of 7 cells, the fixed progress fields (running calls, plan step, queue), and then free text to the end of the row. Nothing is anchored to the right in the live row, so free text cannot push a field. The status band has a right group. Its free text (the product name in the right group, and the model and folder names in the left group) is cut to a fixed number of cells with `fit`, so the right group keeps its place.

#### Resize mechanics

- The cursor is on the composer row that holds the insertion point. A resize calls `scr.Erase()` (`BT/cursed_renderer.go:690`), so the next render runs `clearUpdate` (`UV/terminal_renderer.go:1493-1495`). The renderer moves up by the cursor row, then erases from the frame's first row to the end of the screen (`clearBelow`, `UV/terminal_renderer.go:1273-1276`), and paints the dock at the new width. History lies above the frame and is not touched.
- Old rows can stay on screen once, and only as composer text. The renderer moves from the cursor row it has recorded, so a composer row above the cursor that the terminal rewrapped is not erased. The same holds when the text before the cursor on its row is longer than the new width, because the cursor's physical row is then lower than the renderer expects. The next history row is printed below the stale rows. The live session records both cases.
- The client never reprints history after a resize, and it never clears.

#### Names that several steps share

- `internal/tui/width.go`: three methods on `screenState`. `cells(text string) int` measures with `screen.method`. `fit(text string, n int) string` cuts to `n` cells and ends a cut text with `...`. `wrap(text string, n int) []string` wraps at `n` cells. Each applies NFC first (cross-phase decision 57).
- `internal/tui/dock.go`: `func (m *model) dockLines() []string` builds the dock in the order above. `func panelRows(kind panelKind, windowHeight int) int` gives the fixed height of a panel. `const liveStateCells = 8`, `const elapsedCells = 7`.
- `internal/tui/blocks.go`: `type blockSplitter` with `func (s *blockSplitter) Feed(delta string) (final []block, tail string)` and `func (s *blockSplitter) Close() []string`. `final` holds each block whose rendering can no longer change, as a `block` with its text and its `code` flag. `tail` is the unfinished rest.
- `internal/tui/rows.go`: `func (m *model) commit(key string, lines []string)` queues one block and returns nothing. `takeChunk` wraps each line to at most `m.screen.width - 1` cells, not `commit`. Nothing prints until `Update` calls `drain`. `drain` prints the queue in chunks of at most `m.screen.height - len(m.dockLines())` rows, one print in flight at a time, and only on a settled frame (print rule 2).
- `m.leaving`: set in the update that starts a quit or a suspend; `tea.Quit` follows later from `finishQuit`, and `tea.Suspend` follows later from `finishSuspend`. While it is set, `View()` returns one blank row.

#### Difference from the current design

| Today in `internal/tui` | Under this design |
|---|---|
| Streamed text grows the live item with no limit (`model.go:471-487`, `model.go:684-693`) | Final blocks are printed once; the tail shows on one row |
| An item start adds a live row and its end removes it (`model.go:425-460`) | The live row is always there |
| An approval adds a panel whose height depends on the window width (`model.go:225-237`, `:714-720`) | A panel has a fixed height and sits below the input line |
| Notices, plan lines and the selector add and remove rows with no key press (`model.go:179-181`, `:263-266`, `:500-513`, `commands.go:510-553`) | A notice uses the live row; a plan change is a history row and a fixed field |
| The status row wraps and grows a suffix (`model.go:672-681`) | The status band is one row of fixed fields |
| The client draws its own cursor glyph and sets no `View.Cursor` (`composer.go:56-62`, `model.go:637-641`) | The terminal's cursor is always set, on the composer row that holds the insertion point |
| The composer is below the live rows | The composer is the first part of the dock |
| Lines are wrapped at the full window width (`render.go:219-225`) | Every row is one cell short of the window width |
| A keyword in the composer changes the row every 100 ms (`model.go:616-631`, `keywords.go:93-120`) | A static mark; one tick chain for the dot and the elapsed time |

The earlier plan text also changes: the rule that the dock shows a streaming item in full; the live row above the composer; text fields inside panels; an empty last view on quit; a plan panel and a queue region that grow.

