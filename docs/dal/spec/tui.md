# TUI

The TUI is a protocol client. It MUST import only `internal/dal/rpc` from this module.

## Entry point

`Run(ctx, conn, opts)` dials `conn` with `rpc.Dial(ctx, conn, handler)`. `rpc.Dial` takes a context as its first argument. Cancelling it ends the handler and request contexts that the client derives from it. The RPC client delivers notifications and server requests in arrival order. Its dispatch queue holds 256 messages. Each message goes to the Bubble Tea program as one message.

`Options` fields:

- `OnStatus func(Status)`: called after each state change and each applied notification. `Status` holds `State`, `Seq`, and `ThreadID`. `Seq` is the highest `seq` seen in `thread/status/changed` for the current thread. That `seq` is the journal sequence of the event that changed the status. `Seq` resets to 0 when the TUI's thread changes, because each thread has its own journal.
- `OnChildSpawned func(childID, cwd string)`: called once for each `dalgona/thread/childSpawned {threadId, childId, cwd}` whose `threadId` is the TUI's thread.
- `Input` and `Output` are terminal streams. Empty fields use the process terminal.
- `ThreadID`: when set, startup calls `thread/resume` with turns included. When empty, startup calls `thread/start`.
- `Model`, `Approval`, `Sandbox`: option values for `thread/start`, `turn/start`, and the status band. An empty model omits `model` from `thread/start`, and the band then shows the model that the server reports. A model chosen with `/model` is sent as `model` on the next `turn/start`, and it applies to that turn and later turns of the thread. Without `--approval`, the TUI MUST omit `approvalPolicy` from `thread/start`, so the configured approval applies. The status band MUST show the approval that the server reports for the thread. The sandbox default is `workspace-write`. Approval `yolo` MUST force sandbox `off`. `thread/start` maps approval `yolo` to `never`, `ask` to `untrusted`, and `auto` to `on-request`. It maps sandbox `off` to `danger-full-access` and other values to `workspace-write`.
- `Cwd`: the client working directory. Every `thread/start` MUST send it as `cwd`.
- `Width`, `Height`: the terminal size when the output is not a terminal. Both MUST be positive to apply.

The `dal` and `dalgona` commands MUST NOT start the interface when stdout is not a terminal. They MUST print `<product>: the interactive interface needs a terminal on stdout; use -p <prompt> for one turn` as one line on stderr, and MUST exit with status 1. The refusal MUST write nothing to stdout and MUST send no request on `conn`. The commands MUST close `conn` after the refusal. The check runs after the commands dial and resolve `-c`, so an error from either replaces the refusal.

Startup runs these requests in order: `initialize` with client name `dalgona`, the `initialized` notification, `dalgona/settings/read`, `dalgona/keywords/list` (once per connection), `model/list`, then the thread open call. When `icons` is `auto` under kitty, the client also sends the terminal query named in Icons. `Run` returns nil when the user quits or `ctx` ends. On return, `Run` MUST close the RPC connection.

After `initialize` succeeds, the client MUST print one welcome row with the product, version, model, and working directory. A home directory MUST show as `~`. The welcome row MUST use only bold and dim text. When `initialize` fails, the client MUST print the welcome row with the error on one line. Quit MUST print one exit row. The exit row MUST name the thread and the `--resume` command, or say that no thread was started. The client MUST print the exit row before it returns `tea.Quit`, unless the window height is 0. While the client leaves, the view MUST be one blank row.

The client MUST paint the product mark at the lower right of the screen when the terminal can show images. Detection MUST read only the terminal environment. `TMUX` or `STY` set selects no mark. An empty or `dumb` `TERM` selects no mark. A `TERM` that names kitty or ghostty, a set `KITTY_WINDOW_ID`, or a set `WEZTERM_PANE` selects the kitty graphics protocol. A `TERM_PROGRAM` of `iTerm.app`, or a set `ITERM_SESSION_ID`, selects inline images. When both apply, the kitty graphics protocol wins. Every other environment MUST show no mark and MUST emit zero control bytes for it. The mark MUST come from the rasterized brand assets embedded in the binary, at 8 rows with aspect preserved. An unknown product MUST show no mark. A terminal without image support shows only the product wordmark in the band.

## Screen

The TUI MUST run inline, except for the review. The review is the only view that MUST use the alternate screen. A completed item MUST print once to scrollback with `tea.Println`. Completed assistant text MUST render as Markdown through Glamour.

Streamed text MUST print each final block once, and the live row MUST show one tail row. The live tail MUST keep the newest 64 KiB of stream text, cut on a UTF-8 boundary. It MUST show the last line of that tail. An incomplete trailing terminal control sequence MUST stay in at most 4 KiB of pending state until its terminator, and MUST NOT reach visible text. Text after the terminator MUST show.

The dock holds, from the top, the composer, an open panel, the live row, and the status band. The live row MUST be one row. From the left, it shows the state tag, elapsed time, running calls, plan progress, and queued count, each in a fixed field. One space MUST separate each field after the elapsed time from the field before it. Free text MUST fit the cells left after the fixed fields, without cutting those fields. Its free text shows the hint, else the settings error. Otherwise it shows the rate-limit countdown while a retry wait runs, else the notice, else the activity.

While a key hint shows, it MUST cover the calls, plan and queued cells, and the counts MUST return when the hint ends.

When the free text would have fewer than 22 cells, every blank field after the last field with text MUST give its cells to it. A field with text MUST keep its cells and its place. A cut free text of fewer than 3 cells MUST show one `.` for each cell.

The activity MUST come from an open question or approval first, then the retry countdown, then a running tool call.
With no running tool call, the activity MUST be the first plan step that is in progress, else the first pending plan step.
The plan step text MUST be the first line of the step after the sanitizer runs.
The client MUST NOT remove leading numbering from the step text.
With no step to show, the activity MUST be the tail of the streaming item, else `Thinking` or `Writing`.

The composer shows `› ` in the thread hue, then the typed text. The terminal cursor marks the insertion point. When the composer is empty, the placeholder follows the prompt in dim text. The placeholder never shows a keyword mark.

States are `idle`, `running`, `awaiting approval`, `awaiting input`, `interrupted`, `failed`, and `disconnected`. Each state MUST appear as text. Color MUST NOT be the only signal. Live text MUST be cut to one row, and a cut text MUST end with `...`. At 40 columns no line may exceed 39 cells.

The state words are `Working`, `Waiting`, `Stopped`, `Failed`, `Offline`, `Ready`, and `Done`. `running` shows `Working`, `awaiting approval` and `awaiting input` show `Waiting`, `interrupted` shows `Stopped`, `disconnected` shows `Offline`, and `idle` is named `Ready`, or `Done` after a completed turn. An idle live row MUST show no state word.

A notice that does not fit the free text of the live row MUST also print once as a row, with the full text.

A thread status object maps to a state. `active` with the flag `waitingOnApproval` gives `awaiting approval`. `active` with the flag `waitingOnUserInput` gives `awaiting input`. Any other `active` gives `running`. `systemError` gives `failed`. `idle` and `notLoaded` give `idle`. A turn status maps as follows: `inProgress` gives `running`, `interrupted` gives `interrupted`, `failed` gives `failed`, and `completed` gives `idle`. The `/resume` list panel shows each thread's mapped state.

Each `/resume` list row shows a date between the title and the state. The date field is 7 cells wide and right aligned in dim text. A timestamp in the current year shows `MM-DD`. A timestamp in any other year shows `YYYY-MM`. The client captures the clock once for the whole list and converts each timestamp to that clock's location. The row reads `updatedAt` first, then `createdAt`. A missing or invalid timestamp leaves the field blank, and the state keeps its column.

An idle status that follows an interrupted or failed turn MUST keep that state until the next turn starts.

The TUI follows one thread. The first thread it sees, from a response or a notification, becomes the TUI's thread. `/new`, `/resume`, and `/fork` replace it. Notifications whose `threadId` names another thread, such as a spawned child, MUST NOT change the title, state, live items, or thread. When a `/new`, `/resume`, or `/fork` load applies, the TUI MUST keep the follow-up queue. It MUST show `<n> follow-ups moved to <title>; press Enter on an empty prompt to send the next` when that load applies and the queue holds at least one follow-up.

A `/resume` or `/fork` load of another thread MUST print a separator row, `── <title> ──`, under the key `thread:<threadId>:<n>`. A `/new` load MUST print the row `started <title>` under the same key form. Earlier items of a long thread MUST print their cut note under the key `earlier:<threadId>:<n>`.

When a read from `conn` fails, the TUI MUST enter `disconnected` and show `connection lost: <cause>; restart the TUI, then /resume <thread>`. A later notification MUST NOT leave `disconnected`.

## Status band

The status band is one row of fixed fields at the bottom of the dock.

The band MUST show these items in this order: `model`, `effort`, `approval`, `sandbox`, `folder`, `queue`, `cost`, `context`, and `mark`. The left group holds `model` through `queue`. The right group holds `cost`, `context`, and `mark`.

- `model` shows the model name as text. The client MUST NOT hide it.
- `effort` shows one glyph for the effort rank. It is hidden when the model has fewer than two effort levels.
- `approval` shows one glyph for `ask`, `auto`, or `yolo`.
- `sandbox` shows one glyph for `workspace-write` or `off`. When the harness sandbox is unavailable, the server reports `dangerFullAccess`, and the client MUST show the `off` glyph.
- `folder` shows the folder name, cut to 16 cells with `...`.
- `queue` shows `+` and the count of queued follow-ups. It is shown only when a follow-up is queued.
- `cost` shows the thread cost in US dollars, with no glyph. It is shown only when `tokenUsage.costUsdMicros` is present.
- `context` shows the percent of the context window in use. In `nerd` mode, a moon gauge comes before the percent.
- `mark` shows the product name.

During an active turn, the effort glyph MUST use `dalgona/turn/effort` for that thread and turn. The client MUST ignore a different thread or turn id. Before resolution, the client MUST hide the glyph. A resolved empty effort MUST hide the glyph, not use the thread default. Active resume MUST restore the optional `turnEffort`, including an empty string. Completion, interruption, failure, and thread switch MUST clear this value. When idle, the band MUST use the unchanged thread default. A model without declared effort levels MUST keep the effort item hidden.

Resume MUST take the active turn id and `turnEffort` from the same `thread/resume` response. It MUST NOT read a second snapshot to obtain the turns. A resume result MUST NOT replace newer turn or effort notifications that the client has already applied.

During a resume request for another thread, the client MUST buffer that thread's notifications.
After it applies the snapshot, the client MUST replay those notifications in receive order.
A superseded resume response or error MUST NOT change the client.
If the buffer exceeds its limit, the resume MUST fail and keep the current thread.

The setting `status_hide` lists items to hide. Its default is empty. The client MUST NOT hide `model`.

The percent of `context` MUST keep four cells, and the count of `queue` MUST keep three cells. The text of `cost` MUST keep seven cells, aligned to the right. A changed value MUST NOT move any other item.

If `tokenUsage.estimatedContextTokens` is present, the percent MUST use that journal estimate, including zero.
Otherwise, the percent MUST use the sum of input, cached input, cache-write, and output tokens in `tokenUsage.last`.
The next provider usage report MUST replace the estimate.

The client MUST round the cost to whole cents, half up, before it picks the form of the text. Below 100 dollars, the text MUST read `$<d>.<cc>`. From 100 dollars to below 10000 dollars, it MUST read `$<whole>`. From 10000 dollars, it MUST read `$9999+`. A partial cost MUST add `+` to the first two forms.

When the row is too narrow, the client MUST drop whole items in this order: `folder`, `queue`, `cost`, `effort`, and `mark`. It MUST NOT drop `model`, `context`, `approval`, or `sandbox`. When those still do not fit, it MUST cut the `model` text with `...`.

## Icons

The setting `icons` MUST be `auto`, `unicode`, or `nerd`. Its default is `auto`.

`auto` MUST start as `unicode`. When `KITTY_WINDOW_ID` is set and `icons` is loaded as `auto`, the client MUST send the capability request `kitty-query-font_family` once. A reply that does not name `kitty-query-font_family` MUST NOT change the glyphs. While `icons` is `auto`, a reply that names a Nerd Font MUST switch the client to `nerd`. A Nerd Font name matches `(?i)nerd ?font` or `NF[MP]?(-|$)`.

| Item | Glyphs |
|---|---|
| effort | `⣀`, `⣤`, `⣶`, `⣿` by rank, at index `round(rank × 3 / (levels − 1))` |
| approval | `?` for `ask`, `✓` for `auto`, `!` for `yolo` |
| sandbox | `⊡` for `workspace-write`, `⊘` for `off` |
| folder | `▸` |
| queue | `+`, then the count |
| context | the percent alone. In `nerd` mode, a moon gauge at index `round(fraction × 14)` from `U+E38D` to `U+E39B`, then the percent |

Each Unicode glyph MUST have East Asian Width `N`, `Na`, or `H`. A Nerd Font glyph counts one cell, and one space follows it.

## Layout

The dock MUST be the last rows of the screen. From the top, it holds the composer, an open panel, the live row, and the status band.

The dock MUST keep its height. Only a key press, a paste, the opening or closing of a panel, or a window resize MAY change it. A failed send or steer that puts its text back in the composer MAY also change it.

The dock MUST have at most the window height minus one rows. The client MUST show no dock row in a window of fewer than two rows.

The composer MUST show at most six rows, or one row while a panel is open. A panel MUST have a fixed height for its kind, capped at the window height minus four rows.

When the dock is too tall, the client MUST drop the status band first, then the live row. The input line MUST NOT drop.

Every dock row MUST be at most the window width minus one cell.

After a geometry change settles, the client MUST request the terminal cursor position.
A valid reply MUST place the dock at the physical bottom with relative cursor moves and line erases.
That placement MUST erase each dock row before it repaints that row.
The client MUST NOT print blank history rows to place the dock.
A stale reply or a changed frame MUST NOT move the dock.
The client MUST request plain cursor-position reports (`ESC[6n`).
After a committed print, the client MUST reject row-one reports and request the position again.
After the client rejects a reply that is not a row-one report, it MUST wait one second before it requests the position again.
A geometry change or a committed print MUST end that wait.
After a committed print, the client MUST return the terminal cursor to the caret within 100 ms.
A cursor query or dock move in flight MAY delay that return until it ends.
Modified F3 key events MUST remain available to the composer.
A relocation MUST finish its output write before the client prints another committed row.
A relocation MUST end one second after it starts when its output write has not finished.
The client MUST request the cursor position again after such an end.
Private output markers MUST NOT reach the terminal.
An unanswered query MUST NOT block input or committed output.
A query MUST expire after one second without a reply.
With a window height above 0, quit and suspend MUST NOT return while a query or a dock move is in flight.
A geometry change during a pending query MUST remain eligible for placement after that query expires.
If geometry stayed unchanged until expiry, the client MUST wait for a geometry change before another query.
Physical-bottom placement requires a terminal that answers cursor-position queries.
On a terminal that does not answer, input and output remain available, but physical-bottom acceptance is not proven.

## Panels

A list panel MUST show its title in the first row and its hint in the last row.
A list panel MUST show at most its panel height minus two item rows.
The list MUST filter its items by the text of the input line.
The list MUST order matching items by match: a prefix, then a substring, then a subsequence.
Printable keys MUST add to the filter while a list panel is open.
Up and Down MUST move the cursor. PgUp and PgDn MUST move it by a page.
Enter MUST choose the highlighted item. Enter MUST do nothing when no item matches.
Esc MUST clear a non-empty filter. Esc MUST close the panel when the filter is empty.
A list panel MUST lose its filter when another panel opens above it.
The input line MUST show the filter while a list panel is open.
The input line MUST show the first line of the draft while another panel is open, with the caret at its end.
A confirm panel MUST show its title, its body, and its action row. It MUST NOT send a request before Enter.
The review target input MUST show `branch:`, `commit:`, or `instructions:` as its label, and MUST hide the filter while it waits for text.

The input line MUST show the mode of the top panel, as the table below gives.

| Mode | Label | Typed text | Enter | Esc |
|---|---|---|---|---|
| inputPrompt | `› ` | The prompt draft | Sends the prompt | Follows the Keys table |
| inputShell | `! ` | A shell command | Runs the command | Leaves shell mode |
| inputFilter | `filter: ` | The list filter | Chooses the highlighted item | Clears the filter, then closes the list panel |
| inputTarget | `branch: `, `commit: `, or `instructions: ` | A branch, a commit, or custom instructions | Sends the review | Clears the text and the pending target. The list stays open |
| inputAnswer | `answer: ` | The Other text or a free-text answer | Keeps the text and moves on | Returns to the option rows, or dismisses a free-text question |
| inputAPIKey | `API key: ` | The key, shown as one `*` for each rune | Sends `account/login/start` | Clears the key and returns to the method rows |

| Kind | Rows |
|---|---|
| popup | 8 |
| list | 12 |
| confirm | 6 |
| approval | 9 |
| ask | 13 |
| settings | 17 |
| login | 12 |
| help | 14 |

The model picker MUST list the preset models first, as `default`, `smol`, and `slow` rows, then every visible model.
Tab and Shift+Tab MUST cycle the effort of the highlighted model.
Enter MUST write `model` with `config/value/write`, and MUST write `model_reasoning_effort` after it when the effort changed.
A failed write MUST keep the model picker open and MUST show `Could not save` in its hint row.

The dalgona settings panel MUST list seven rows in this order: Approval, Verbosity, Theme, Icons, Status, Edit review, and Notifications.
The dal settings panel MUST omit Edit review from display, navigation, and saves. It MUST keep the other rows in that order.
While the Edit review row has focus, the detail area MUST show `Sends each edit's masked diff to the decider model.`. It MUST show no detail on any other row.
Both products MUST keep the settings panel at 17 rows.
Each settings row MUST show its label in the first column and its value after it. The focused row MUST show its label and its value in bold. A value that differs from the saved value MUST use the accent colour. The Status row MUST show `all shown` or `<n> hidden` for its value. Detail lines MUST use dim text.
Up and Down MUST move between rows. Left and Right MUST change the value of a row. On Status, they MUST move between the status items.
The detail area of Status MUST list the status items in columns of seven rows. A new column MUST start when a column holds seven items.
Space MUST toggle a status item on Status, and MUST change the value of any other row.
Theme, verbosity, and icons MUST take effect at once while the panel is open.
Enter MUST save the changed rows in one `config/batchWrite` request. In dalgona, the keyPaths MUST be the wire names `approval`, `verbosity`, `theme`, `icons`, `statusHide`, `editReview`, and `notifications`. In dal, they MUST omit `editReview`.
A failed save MUST keep the settings panel open and MUST show `Could not save settings` in the live row. A key press MUST NOT clear that text. It MUST stay until the panel closes or a save succeeds.
While a save is in flight, Enter MUST send no request.
A successful save that changes approval MUST show `saved; open threads keep their approval and sandbox`. Any other successful save MUST show `Saved to <file>`.
Esc MUST restore the saved values and MUST close the settings panel.
The login panel MUST list the providers from `dalgona/account/providers/list`. `/login <provider>` MUST open it at that provider's methods.
A provider with the `apiKey` method MUST offer an `API key` method. ChatGPT MUST offer `Browser` and `Device code` methods.
A provider that signs in from a shell MUST show the command to run there, and MUST offer no method.
The API key field MUST show one `*` for each rune. The key MUST NOT appear in a row, notice, history, or review.
Enter on the API key field MUST send `account/login/start` with `type` `apiKey`. The panel MUST close when the provider accepts the key.
A refused key MUST keep the field open with the error in the panel. Esc on the field MUST clear it and return to the methods.
An approval request MUST NOT clear the API key field.
A browser or device sign-in MUST show the link, and a device sign-in MUST show the user code. It MUST wait for `account/login/completed`.
Esc during a sign-in MUST send `account/login/cancel` with its `loginId`, and MUST close the panel. Esc before the start call answers MUST keep the panel open with `Cancelling sign-in…`. When the answer arrives, the client MUST send `account/login/cancel` with its `loginId`, MUST show no sign-in link, and MUST close the panel.
A paste into the API key field MUST keep its first line and MUST NOT make a chip.

## Print

The client MUST call `tea.Println` only from `drain`, and MUST have at most one print in flight.

The client MUST start a print only after the dock height, the window size, and the alternate screen state have not changed for three frames. A change of text inside the dock MUST NOT reset that wait.

The client MUST keep the view frozen while a print is in flight. A window resize MUST rebuild the frozen frame at the new size.

A print MUST hold at most the window rows minus the dock rows.

The client MUST show an empty view until its first row has printed.

A failed `initialize` MUST print one row that starts with `Could not start:`.

The client MUST print one thread row for every thread load, also when the same thread loads again. A reload MUST NOT print an item row again.

With a window height of 0, quit and suspend MUST return at once and MAY drop the rows that wait.

A tool call nested in an eval call MUST print as its own row before the row of the eval call.

The client cannot see a failed write to the terminal. A write that fails is therefore not reported.

## Review

`ctrl+o` MUST open the review on the newest entry. The review MUST show the full text of an entry, not its cut form.
The review MUST close only on `esc`, `q`, or `ctrl+o`. A quit or suspend key MUST close the review first.
Rows that finish while the review is open MUST wait, and they MUST print after the review closes.
The review header MUST show the count of waiting rows. The footer MUST name the scroll keys and the entry keys.
The review MUST have at most the larger of one row and the window height minus one rows. The review MUST keep one body row.
A window of fewer than four rows MUST drop the review footer. A window of fewer than three rows MUST drop the review header.
The client MUST keep the last 1000 committed rows for the review and MUST drop the oldest first.

## Mouse

The client MUST enable mouse reporting only while the review or a list panel is open. Otherwise the terminal keeps its own selection and scrollback.
The wheel MUST scroll the review. With a list panel open, the wheel MUST move its window three rows.
A list panel MUST move its cursor to the nearest visible row when the wheel leaves that row out of view.
Other panels MUST ignore the wheel.
A click MUST select nothing, because the client cannot learn the screen row of the dock.

## Editor

The composer MUST wrap text to the window width minus three cells.
With no panel open, each row of the wrapped text MUST take one composer row.
With no panel open, the composer MUST show the caret row. When the caret row is past the last row that fits, the composer MUST show the rows that end with it.
Enter MUST send the prompt, except after a backslash and while the command palette lists a command.
Enter after a backslash MUST replace the backslash with a newline.
Shift+Enter and Ctrl+J MUST insert a newline.
Alt+Enter MUST queue the draft as a follow-up.
Up MUST move the caret between wrapped rows. On the first row, Up MUST recall the previous history entry.
Down MUST move the caret between wrapped rows. On the last row, Down MUST recall the next entry, and past the newest entry it MUST restore the draft with its chips.
Home and End MUST move the caret to the start or end of its line.
Alt+Left and Alt+Right MUST move the caret by word.
Ctrl+W MUST delete the word before the caret.
Ctrl+U MUST delete from the start of the line to the caret.
Ctrl+K MUST delete from the caret to the end of the line.
The history MUST hold the 200 newest prompts. It MUST skip a prompt equal to the one before it, and it MUST NOT hold a `/login` line.
Ctrl+R MUST open a list panel titled `history` that lists the sent prompts, newest first.
Ctrl+R MUST open no panel in shell mode.
Each row MUST show its prompt on one line, with each run of whitespace collapsed to one space.
A row MUST show at most the first 1024 bytes of its prompt.
Enter MUST replace the draft with the full text of the highlighted prompt.
Enter MUST close the panel.
Delete MUST remove the highlighted prompt from the history, with or without a filter.
Backspace MUST remove the highlighted prompt when the filter is empty.
Each entry of the history MUST carry a number that no later entry reuses.
Enter and Delete MUST act on the entry whose number the highlighted row holds.
When that entry is gone, Enter and Delete MUST do nothing.
The panel MUST then rebuild its rows from the history.
A new prompt, a removal, or the 200 prompt cap MUST refresh an open history panel.
The refresh MUST keep the highlight on the same entry while that entry exists.
A removal MUST reset Up and Down recall to the newest end of the history.
A removal MUST NOT change the draft in the editor.
Removing the prompt that waits for its turn MUST NOT move that turn to another entry.
Closing the panel with Esc MUST leave the draft unchanged.
With no history, the panel MUST show `No prompt history yet.` and no selectable row.
With no history, Enter, Delete, and Backspace MUST do nothing.
A draft that holds only whitespace MUST count as empty, for Esc and for Ctrl+C.
Ctrl+C with a draft that holds text MUST move the draft into history. It MUST show `Draft saved to history · Ctrl+C again to exit` in the live row.
An image chip MUST NOT enter the history. When the draft holds an image, the live row MUST read `Draft saved to history, images dropped · Ctrl+C again to exit`.
Ctrl+C with a draft that holds no text MUST keep the draft. It MUST show `Ctrl+C again to exit` in the live row.
The client MUST set the terminal cursor at the caret and MUST NOT draw a cursor glyph.
A paste MUST turn each CRLF, each CR, and each LF into one line break.
A paste into a one-line field MUST keep only its first line.
The one-line fields are the list filter, the review target, the answer line, and the API key field.
A paste of fewer than 10 lines MUST be inserted as text.
A paste of 10 or more lines MUST become one chip labelled `[Paste #n +L lines]`, where L counts its lines.
A chip MUST take one composer row, whatever the length of its paste.
Backspace and Delete MUST remove a whole chip.
Sending MUST replace each chip with its full pasted text, in order.
The history MUST store each prompt, and each draft that Ctrl+C saves, with each chip that holds text replaced by its full text.
A prompt recalled from the history MUST be plain text. It MUST NOT become a chip.
A recalled prompt whose draft began with a chip MUST be sent unchanged until the user edits its text.
The slash rule MUST read only the typed text. A pasted line that starts with `/` or `//` MUST be sent unchanged.
A paste over 1 MiB MUST be refused with a notice that names the limit.
Ctrl+V MUST paste an image from the clipboard through `wl-paste`, `xclip`, `pngpaste`, or PowerShell.
A pasted path to a PNG, JPEG, or GIF file MUST become an image chip labelled `[Image #n WxH]`.
A path followed by one line break MUST also become an image chip, and the chip MUST replace that line break.
A paste with a line break before its last character MUST stay text, even when its lines joined name an image file.
A pasted path to a WebP file MUST be refused with the notice `unsupported image type image/webp; send PNG, JPEG or GIF`.
An image chip MUST take one composer row.
A prompt MUST hold at most 10 images and at most 10485760 bytes of image data.
An image over 5242880 bytes MUST be refused with a notice that names the file and its size.
A model without image input MUST refuse an image paste with the notice `this model does not accept images`.
Sending MUST send each image chip as an image input item after the text.

The composer MUST show a command palette when the caret is in a first token that starts with `/`, with no panel or review open.
The palette MUST list at most 8 matching commands and MUST NOT open for `//`.
Up and Down MUST move the highlight. With no matching command, Up and Down MUST act on the composer. Tab MUST complete the highlighted command to `/name `, and it MUST keep the text after the token. It MUST keep each chip of the draft. The palette MUST end the first token at a chip.
Enter MUST run a highlighted command only when the draft is only its first token and the command takes no argument. Otherwise, Enter MUST complete the command and keep the rest of the draft.
Esc MUST close the palette and keep the draft. Typing a character MUST reopen it.

The composer MUST show a file palette when the caret is in a token that starts with `@`, with no panel or review open.
The file palette MUST search for the text after `@` 120 ms after the last keystroke, and it MUST list at most 8 paths.
Tab and Enter MUST replace the token with `@<path> `. Esc MUST close the file palette and keep the draft.
A change of the file query MUST clear the rows of the previous query. The palette MUST NOT insert a path from an older query.
A file palette with no rows MUST consume Enter and MUST NOT send the draft.
When the file search is missing on the server, the first such palette MUST show `File search is unavailable on this server`.
The file search MUST use the open thread's directory as its only root.
Before a thread opens, the file search MUST use the launch directory.
Enter in shell mode MUST run the line with `command/exec`, MUST add its output to the prompt as a chip, and MUST keep shell mode.
The client MUST allow a shell command its 120 second timeout before it reports a failure.
The chip MUST quote the output in a fence longer than any run of backticks in the output.
Esc on a shell line with no typed text MUST leave shell mode.
A shell line with only chips has no typed text. Esc on it MUST keep the chips in the prompt.
A shell command MUST run in the open thread's directory. Before a thread opens, it MUST run in the launch directory.
The folder item, its help legend and the welcome details MUST show the open thread's directory.
Before a thread opens, they MUST show the launch directory.

## Trademark logo

When the terminal supports an inline image protocol, the client MUST paint the product's trademark logo.
The mark MUST sit at the lower right of the screen, under text, at reduced opacity.
The client MUST use the kitty graphics protocol on kitty, ghostty, and WezTerm, and the iTerm2 inline image on iTerm2.
A `dalgona` session MUST show `assets/brand/dalgona` and a `dal` session MUST show `assets/brand/dal` tinted to the accent colour.
The mark MUST be translucent so text stays readable over it. Kitty MUST draw the mark below text with a negative z index.
TMUX or STY set selects no logo. A terminal without image support MUST show the product name through the band `mark` item instead.
Kitty emission MUST hold the cursor with `C=1`, reuse one image id across repaints, and chunk base64 at 4096 bytes.
The mark MUST preserve the source aspect ratio and MUST repaint at the lower right on every frame.
The iTerm2 emission MUST follow the iTerm2 inline images protocol with cell `width` and `height`.

## Keys and commands

| Input | Effect |
|---|---|
| Enter, idle | `turn/start` with the composer text, unless the command palette lists a command. The palette then runs or completes that command |
| Enter, running or awaiting approval | `turn/steer` with the composer text |
| Enter, empty composer | Starts the oldest held follow-up when no turn runs |
| Alt+Enter | Queues a follow-up, at most 32 |
| Alt+Q | Opens the queue panel, which lists each queued follow-up as `<i>. <first line>` |
| Alt+Up, empty composer | Moves the newest queued follow-up back into the composer |
| `?`, empty composer | Opens the help panel. It lists each key with its action, then one status line for each shown status item. `?` or Esc closes it. Up and Down scroll one line, and PgUp and PgDn scroll a page |
| `/help` | Opens the help panel |
| Esc | Closes an open command palette first and keeps the draft. Otherwise clears a non-empty list filter first. With an empty filter, closes the list panel. While a turn runs or waits, the first Esc shows `Esc again to interrupt` in the live row. A second Esc within 2000 ms sends `turn/interrupt` on the active turn. One Esc interrupts during a rate-limit wait. Any other key clears the pending hint |
| Esc on an empty prompt, while idle | The first Esc shows `Esc again to rewind` in the live row. A second Esc within 2000 ms opens the rewind list of active turns, newest first. |
| Esc, with no turn to interrupt | After a stopped turn the notice reads `stopped; nothing to interrupt`. After a failed turn it reads `failed; nothing to interrupt`. After a lost connection Esc shows the `connection lost` notice again. In an idle prompt with a draft it reads `idle, draft kept; nothing to interrupt`, the draft stays, and no rewind hint shows. In these states Esc sends no `turn/interrupt`, also during a rate-limit wait |
| Ctrl+C twice | Quits. With a draft that holds no text, the first press keeps the draft and shows `Ctrl+C again to exit` in the live row. With a draft that holds text, the first press moves the draft into history and shows `Draft saved to history · Ctrl+C again to exit`. When that draft holds an image, the live row shows `Draft saved to history, images dropped · Ctrl+C again to exit`. A second press within 2000 ms quits |
| Ctrl+R | Opens the history panel of sent prompts. Enter loads the highlighted prompt into the editor. Delete removes it. Shell mode ignores Ctrl+R |
| `/model [id]` | `model/list` list panel, or `config/value/write` key `model` |
| `/resume [id]` | `thread/list` list panel, or `thread/resume` with turns included |
| `/new` | Opens a confirm panel titled `Start a new thread?`. Its body reads `<n> follow-ups move to it` when the queue holds follow-ups. Enter calls `thread/start`. Esc cancels |
| `/compact` | Opens a confirm panel titled `Compact the context?`. Its body reads `ctx <pct>% of <window> tokens` when the window is known. Enter calls `thread/compact/start`. Esc cancels |
| `/login [provider]` | Login panel. `dalgona/account/providers/list` loads the providers. Its methods call `account/login/start` |
| `/tree` | `dalgona/thread/tree` list panel. Enter calls `dalgona/thread/leaf/set` |
| `/revert` | Active-path turns from `dalgona/thread/tree`. Enter opens a confirm panel, and its Enter calls `thread/revert`. Esc keeps the turn |
| `/fork` | `dalgona/thread/tree` lists the active-path turns, newest first. Enter calls `thread/fork` with the highlighted turn as `lastTurnId`, then resumes the new thread |
| `/advisor` | `dalgona/advisor/status` opens an advisor list. Enter calls `dalgona/advisor/set` for the highlighted advisor, enabling it when disabled and disabling it otherwise. The notice says `advisor on` or `advisor off`. The list closes after a toggle |
| `/advisor status` | `dalgona/advisor/status`. It writes one notice line per advisor with its name, status, model, notes, and token usage |
| `/advisor reset` | `dalgona/advisor/reset`. This command is the only way to reset the advisor. Ctrl+R does not reset it |
| `/review` | `review/start`. The list `Review changes` offers `Uncommitted changes`, which starts at once. `Base branch`, `Commit`, and `Custom` take the text of the input line before Enter sends it. The review runs as a turn on the thread |
| `//text` | Sends `/text` as a prompt. A first token that holds a second `/`, such as a path, sends the whole text as a prompt |

While a turn is working or waiting, `/new`, `/resume`, `/fork`, `/compact`, `/tree`, `/revert`, and `/review` MUST NOT send a request. The TUI MUST show `/<name> is unavailable while a turn is <word>; wait for it to finish or interrupt it first`, where `<word>` is `working`, `waiting for approval`, or `waiting for an answer`.

`turn/steer` sends `expectedTurnId` set to the active turn.

When the queue holds 32 follow-ups, Alt+Enter MUST show `follow-up queue is full (32); wait for the active turn or press Esc`. After a follow-up is queued, the notice MUST show `queued follow-up <n> of 32`. An unknown slash command MUST stay in the editor and MUST show `unknown command /<name>; start with // to send it as a prompt`. After `/advisor reset`, the notice MUST show `advisor reset; it reviews the current branch at the next turn`.

A follow-up starts after `turn/completed` only when the turn ends idle. After an interrupted or failed turn, the queue MUST stay held and the notice MUST say how to release it. After a revert, a leaf change, or `thread/reverted`, the TUI MUST show `files changed by later turns are not reverted`.

The interrupt notice MUST end when an interrupted turn ends.

## Approvals

`item/commandExecution/requestApproval` and `item/fileChange/requestApproval` open the approval card. Requests queue in arrival order, and the card shows the oldest one. A request that joins a non-empty queue MUST NOT change the text on the input line. The title is `Approval · Run command` or `Approval · Apply file changes`, followed by ` (1 of <n>)` when more requests wait, and the word `Waiting`.

The body holds at most six rows and scrolls with `up` and `down`. It shows:

- `From thread <id>` when the request comes from another thread.
- `$ <command>` when the request has a command, and `cwd <cwd>` when it names a working directory.
- `reason: <reason>` when the request has a reason.
- For a file change, one line `<label> <path> +<added> −<deleted>` for each file, then its diff from the first hunk on. The diff comes from `item/fileChange/patchUpdated` for the same item. A change with `diffOmitted` shows `diff omitted, larger than 1 MiB` instead of its diff.

The two action rows are `[y] accept  [a] accept for session` and `[n] decline  [esc] cancel and interrupt`. `[y] accept` carries the one fill of the view, in the thread hue. Before the terminal reports its background, the fill MUST use the night theme, or the soft night theme for `auto-soft`. With colour off, `[y] accept` MUST be bold reverse video with no colour.

Ctrl+O opens the full review of the card, with the same lines and the full diff.

When another window settles a request, the TUI MUST remove the card. It MUST show `Approval answered in another window` for `answered`, `Approval dismissed in another window` for `cancelled`, and `Approval closed: the turn was interrupted` for `interrupted`. Any other resolution MUST show `Approval closed: the agent continues without an answer`.

The keys answer `accept`, `acceptForSession`, and `decline`. `n` declines at once, and the response carries no reason. Esc answers `cancel` and interrupts the turn. Each request MUST block until a key answers it, the TUI exits, the connection is lost, or the RPC context ends. A new request MUST NOT replace one that waits. Other server requests MUST fail with an error.

A server request whose parameters do not decode MUST receive an error answer. The TUI MUST show `approval request failed: <cause>; reconnect and retry the turn`.

## Notifications

The TUI applies these notifications: `thread/started`, `thread/status/changed`, `thread/tokenUsage/updated`, `thread/name/updated` (`threadName`), `thread/reverted`, `turn/started`, `turn/completed`, `turn/plan/updated`, and `dalgona/retry/wait`. It also applies `item/started`, `item/completed`, the three delta notifications, `item/fileChange/patchUpdated`, `serverRequest/resolved`, `error`, `dalgona/thread/childSpawned`, and `dalgona/advisor/note`. The TUI also applies `dalgona/tool/judged`, `dalgona/turn/judged`, and `account/login/completed`. `account/login/completed` shows `Signed in to <provider>. Run /model to choose a model` on success, and `Login failed: <error>; run /login again` on failure. It ignores unknown methods. The TUI MUST ignore a `thread/status/changed` whose `seq` is lower than the last seen `seq`. `turn/started` clears the notice line.

While the TUI tracks a turn, it MUST ignore a `turn/completed`, a `dalgona/retry/wait`, or an item delta that names a different turn.

A `/login` response without a URL or a code MUST NOT change the notice line. Its outcome arrives in `account/login/completed`.
A failed login without error text MUST show `Login failed: unknown error; run /login again`.

`turn/plan/updated` MUST print one plan row for each change. The row key MUST be `plan:<turnId>:<n>`, where `n` counts the plan rows of the turn from zero.
The row MUST start with `Plan`, then one line for each step: `Done <step>`, `Working <step>`, or `Pending <step>`.
The row MUST show at most five steps, followed by `+N more` when more steps exist.
The live row MUST show the plan progress as a field, and `turn/completed` MUST clear that field.

Deltas append to the live item with the same id. `item/started` and `item/completed` MUST keep text and output already streamed.

## Subagents

A running `task` call MUST show `Subagent` as its activity, and the calls field MUST count it.
The `dalgona/thread/childSpawned` notification carries no call id, so the client MUST attach the child ID to the newest running `task` call. A finished `task` call MUST NOT receive a child ID.
At `medium` and `high`, a finished `task` call MUST print one `Subagent` row, and the row MUST list each child as `child <id> · /resume <id>`.
At `low`, the `task` call MUST be counted in the open group line instead.
The row MUST show the first line of the `data` member, or `error: <text>` from the `error` member, of the call result.
Rows loaded from thread history MUST NOT list child IDs, because the thread read carries none.

Decoding works as follows:

- The three delta notifications are the hot path. They are read with `encoding/json/jsontext` tokens and unquoted into one reused buffer. The text is then appended to per-item byte buffers.
- Other notifications and responses decode through the `internal/dal/rpc` types with `encoding/json/v2`.
- Unknown fields MUST be ignored.

Settled status labels show `inProgress` as `Working`. Other statuses show as sent.

The TUI also applies `dalgona/tool/judged` and `dalgona/turn/judged`. A judged notification MUST print at most one row. Its key MUST be `review:<threadId>:<turnId>:<itemId>` for a tool verdict, and `review:<threadId>:<turnId>` for a turn verdict. The row MUST read `review`, two spaces, the summary, two spaces, and the paths joined with `, `. The row MUST show at most three paths. More paths MUST end the row with `+<n> more`. A `flagged` row MUST use the warning text role. An `error` row MUST use bold failed text. An `uncertain` row MUST use dim text, and it MUST print only at verbosity `high`. A `clean` verdict MUST print no row. These notifications are live only, so `thread/read` history MUST NOT show a review row.

A judged row MUST add attribution lines when the notification names an answering model. The lines MUST start under the summary and MUST wrap within the window. The text MUST read `judged by <model>`. A notification with `fallback` MUST add `(fallback from <fallback>)` after the model. The `fallback` text names the primary model and its failure. A notification with `judges` MUST list each distinct answering model in part order, separated by `, `. The row MUST NOT show `mixed` as a model. An `error` row MUST read `judge failed: <model>` in place of `judged by <model>`. A `by` of `judge` names no model. The row MUST then add no attribution lines. The TUI MUST sanitize the attribution text and MUST replace each line break in it with a space.

## Title and notifications

The window title MUST read `<product> · <title> · <state word>`. The TUI MUST request focus reports.

A desktop notification MUST be sent only while the window is blurred and the `notifications` setting is true. The TUI MUST send a bell when `TMUX` is set. Otherwise it MUST send OSC 99 when `TERM` is `xterm-kitty`, OSC 9 when `TERM_PROGRAM` is `iTerm.app`, `WezTerm`, or `ghostty`, and a bell.

A turn end MUST notify with `<product>: <word> · <title>`, where the word is `Done`, `Failed`, or `Stopped`.

An approval request for the followed thread MUST notify with `<product>: Waiting for approval · <title>`.

## Advisor notes

`dalgona/advisor/note` prints one faint card to scrollback: `advisor · <severity>: <note>`. When the advisor's name is not `advisor`, the card starts `advisor <name> · <severity>: <note>`. The severity is always text.

Each note also arrives as a `dynamicToolCall` item with tool `advisor`. That item MUST NOT appear among the live items. On completion it MUST NOT print, so each note prints once. In history from `thread/read`, the item prints as the same card. Its severity and advisor name come from `arguments`, and the note comes from the output. An item with unreadable arguments prints as a plain tool item.

## Failures

Every error notice MUST name a recovery action.

- A malformed notification shows `protocol error in <method>: <cause>; reconnect with /resume <thread> or use /new`.
- An `error` notification with `willRetry: false` sets `failed` and shows `error: <message>; retry the turn, /resume <thread>, or /new`. With `willRetry: true`, the state is unchanged and the notice is `error: <message>; retrying the turn, no action needed`.
- With no thread yet, `/resume <thread>` in these notices reads `/resume`.
- An advisor command that fails with `-32001` shows `advisor unavailable: <message>; set [advisor] enabled = true in config.toml, then restart dalgona`. Any other advisor failure shows `advisor request failed: <cause>; retry the /advisor command`.
- A failed `turn/start` or `turn/steer` returns its text to the composer ahead of any newer draft. A failed follow-up returns to the head of the queue. A failed `turn/start` sets `failed`. A failed steer keeps the turn state.
- A prompt sent before a thread exists stays in the composer with `thread is not ready; wait for startup or use /new to retry`.
- A failed keyword list MUST NOT block prompting.
- An interrupt result for an earlier turn MUST NOT change the current state.
- A lost connection MUST end a rate-limit wait and MUST clear a pending key hint. It MUST keep the follow-up queue and the composer draft.
- A lost connection MUST close every open approval and question card. It MUST answer each open request with an error.
- Esc in `disconnected` MUST show the `connection lost` notice again. It MUST NOT send `turn/interrupt`.

## Untrusted text

The client MUST pass every server, model, paste, and command text through one sanitizer before it reaches the screen.
The sanitizer MUST remove escape sequences and control bytes, except newline and tab.
It MUST also remove DEL, C1 controls, bidirectional controls, and private-use runes from U+F0000 to U+FFFFD.
The sanitizer MUST replace each byte that is not valid UTF-8 with U+FFFD. A raw byte from 0x80 to 0x9F MUST NOT pass as a C1 control.

- Each delta of reasoning and command output text, as it is appended.
- The agent message text, sanitized after each delta from all deltas received so far.
- The text, command, working directory, tool, server, status, file change path, and diff of an item, when it starts or completes.
- The thread name, title, and id.
- Each plan step.
- Each error message.
- Each advisor note, with its advisor name and severity.
- The display name of a model, the title of a thread, and the preview of a turn, in a picker.
- The thread id, command, and reason of an approval request.
- Pasted text and typed text in the composer.
- The URL and code of a login.

## Width

The client MUST measure text with the width method of its renderer.
A tab MUST count as the cells to the next 8-cell stop, counted from the first cell of the text that holds it.
A newline MUST restart the count of stops.
A row that the client prints or draws MUST hold spaces in place of each tab.
The composer MUST count its stops from its first text cell after the prompt.
A wrapped row MUST continue the count of stops from the row before it.
The cursor MUST sit after the spaces of a tab that precedes the caret.
A user whose terminal draws East Asian Ambiguous characters two cells wide MUST set `RUNEWIDTH_EASTASIAN=1`.

## Theme

The `theme` setting MUST be `auto`, `auto-soft`, `day`, `soft-day`, `night`, or `soft-night`.
`auto` MUST show `night` on a dark terminal and `day` on a light one.
`auto-soft` MUST show `soft-night` on a dark terminal and `soft-day` on a light one.
The client MUST learn the background from the terminal's reply to a background colour request.
Until that reply arrives, `auto` and `auto-soft` MUST show the night theme.
`NO_COLOR` with any non-empty value MUST turn colour off.
With colour off, the client MUST keep bold, dim, and reverse video, and MUST NOT emit colour.
Without colour, Markdown MUST use the no-TTY style.
Emphasis MUST keep its `*` markers, strong text its `**` markers, and inline code no marker.
The client MUST paint text in the text, dim, accent-text, and failed-text roles.
An error notice MUST be bold in the failed-text role.
The client MUST paint the `Offline` state word in the warning-text role.
The client MUST paint the `Failed` state word in the live tag in bold failed-text, with no fill.
The client MUST paint added diff lines in the added-text role, and removed diff lines in the failed-text role.
A row written before the theme is known MUST NOT use a colour.
Before the theme is known, accent-text and failed-text MUST be bold.
A theme change MUST NOT rewrite a written row.

## Transcript rows

A finished item MUST print once as a row.
The client MUST identify an item by its thread, turn, and item ID.
The same item ID in a later turn MUST print as a new row.
A replay of a printed turn and item MUST NOT print again.
An agent message MUST print as Markdown, with no row.
The state column MUST start with a mark or two blank cells.
The state-word field MUST be at least 8 cells wide and end with a space.
The label MUST be at most 9 cells, and one space MUST follow it.
The meta field MUST be 9 cells, and the duration MUST be 8 cells, right aligned.
The subject MUST follow the label and its space, or the state column when no label shows.
Body lines MUST start under the subject and wrap inside the rest of the row.
A tool output line shaped `[<path>#<tag>]` or `[<path>@<ref>]`, where `<tag>` is a hashline tag and `<ref>` is a snapshot reference, MUST NOT print.
A row MUST end with one blank line.
The state word MUST be `Done` for `completed`, `Failed` for `failed`, `Declined` for `declined`, and `Working` for `inProgress`.
A failed row MUST show `Failed` in bold failed-text, with no background.
A successful `todo` call MUST NOT print a tool row.
At `medium` and `high`, a nested tool call MUST print its own row under its `<run>/<n>` item ID.
At `medium`, an `eval` item MUST print one line with its code line count and its state word.
At `high`, an `eval` item MUST print its first code line as its subject and its output as body lines.
A file change MUST print one row for each file, with its added and removed counts.
A file change whose diff the server omitted MUST show `diff omitted` in place of its counts.

## Diagrams

A fenced code block with info `mermaid`, `d2`, `nomnoml`, or `svg` MUST print as a diagram card, not as code lines.
The fence MUST buffer until its closing marker before it prints.
A mermaid, d2, or nomnoml card MUST render the source as text art under a `diagram <kind> · <n> rows` header.
An `svg` fence or a failed render MUST print `diagram <kind>: render failed · source shown` followed by the source.
A fenced block still open when the answer finishes MUST print its buffered lines as code.
A diagram renderer MUST NOT write its logs to the terminal.

## Verbosity

The `verbosity` setting MUST be `low`, `medium`, or `high`, and its default MUST be `medium`.
At `high`, the client MUST write every block of an item in full, except for a failed item.
At `medium`, the client MUST print each item as its own row.
At `medium`, the client MUST write reasoning as one line, `Thinking · N lines · ctrl+o`.
At `medium`, the client MUST keep the last 10 lines of command output, after a line `… N earlier lines`.
At `medium`, the client MUST keep 60 lines of each file diff, then a line `… N more lines · ctrl+o`.
At `low`, a successful file change MUST print no diff. It counts only in the open group line.
At `low`, the client MUST write reasoning as one line, as at `medium`.
At `low`, the client MUST hold each successful tool item of a turn in an open group, except failed items.
At `low`, the client MUST write the open group as one line.
The line MUST read `Ran N commands · edited M files · called K tools`.
The open group MUST print when an agent message starts or the turn completes.
A failed item MUST keep its last 10 lines at every level.
A failed item MUST write the open group before its own row.
`ctrl+o` MUST open the full text of an item at every level.

## Motion

The client MUST run one tick chain for motion. The chain MUST run only while a turn is live or a rate-limit wait is open.
The live dot MUST alternate between U+25AA and U+25AB every 500 milliseconds.
With `DAL_NO_MOTION` set to a non-empty value, the client MUST keep the dot on U+25AA and MUST tick once a second.
The elapsed time MUST update once a second, also with `DAL_NO_MOTION` set.
A rate-limit wait MUST show `rate limited · retrying in <m>m <ss>s · esc stops`, updated each second.
The countdown MUST clamp at `0m 00s`. Only the matching zero report, the end of the turn, or a lost connection MUST close the wait.
Elapsed text MUST keep its cells, so a change of value moves no other field.

The elapsed time MUST show `<n> ms` under one second, `<s>s` under one minute, `<m>m <ss>s` under one hour, and `<h>h <mm>m` from one hour. Above 99 hours and 59 minutes it MUST show `99h 59m`.

## Keyword mark

The server finds keywords. The TUI MUST NOT match keywords itself. The rules are in [keywords.md](keywords.md).

The TUI MUST send `dalgona/keywords/match` once, 120 ms after the last change of the draft. A newer change MUST cancel the send that still waits. The wait is one timer and starts no tick chain. The TUI MUST NOT send a draft over 64 KiB, which the server refuses under [protocol.md](protocol.md).

The composer MUST show marks only for the draft that the newest accepted `dalgona/keywords/match` reply matched. A reply MUST be dropped when it is not the newest request or its text differs from the draft.

A keyword mark is bold. It is also underlined once the theme is known. It MUST take the accent color only when color is on. The mark MUST NOT change the visible text, and it MUST NOT depend on color alone.

A completed user message MUST show the marks listed in its `keywords` member. A cursor inside a marked word MUST split the mark around the cursor. The mark has no animation and schedules no tick.

## Questions

A `item/tool/requestUserInput` request joins the decision queue behind earlier approvals and questions. The card shows the oldest open decision. The TUI MUST show one question card for each request. Its title reads `Question <i> of <n> · <header>`.

While a question waits, the live row MUST say `Waiting for your answer`.

Rows mode moves with `up`, `k`, `down`, and `j`. `tab`, `right`, and `l` move to the next tab. `shift+tab`, `left`, and `h` move back. Review is the final tab. Forward navigation from Review returns to the first question. Reverse navigation from the first question opens Review.

Digits pick an option. `space` toggles a multi-select option. In a single-select question, `enter` picks the option under the cursor and advances. In a multi-select question, `enter` advances without changing the picks. On the last question, `enter` opens Review without sending answers. On Review, `enter` submits. In rows mode, `s` submits from every tab. `esc` dismisses the card.

Review shows each question header in bold and its answers in option order, joined with `; `. It shows `(no answer)` in dim text for an unanswered question. Its title reads `Review · <n> questions`, or `Review · 1 question`. Arrow keys scroll a Review body that does not fit. Returning to a question keeps its answers for editing. Entering Review resets its scroll position.

An empty question request returns an empty answer map without opening a card.

The Other row opens typing. Typing uses the input line with the label `answer: `. `enter` stores the typed text and advances. `esc` leaves typing, or dismisses a free-text question. An `s` entered while typing remains answer text and does not submit.
A question card that becomes the oldest request MUST show only its own typed text on the input line.

With two or more rows, the card keeps its title and its hint rows. With one row it shows the title alone. At window height 4 or less the card is not drawn. The hint rows list `↑/↓ move`, `1-7 pick`, `Space toggle`, `Enter choose`, and `Tab next`, then `s submit · Esc dismiss`. The hint rows MUST wrap at whole hints and MUST keep `s submit · Esc dismiss` on one row. They MUST take their rows before the body and MUST leave the title and the option under the cursor. After that, the preview drops first, then the question text, then the options scroll. If the rows left cannot hold every hint, the card MUST drop whole hints and MUST keep both actions. A single-select question drops `Space toggle` first. A multi-select question drops `Space toggle` last.

The preview shows the description and the first line of the preview of the option under the cursor. A preview with more lines MUST end its row with `… <n> more lines · ctrl+o`. The full-screen review opened with `ctrl+o` shows every question, every option, and the full preview of each option. `esc` returns to the card at the same cursor row and sends no answer. This full-screen review is separate from the answer Review tab.

The `s submit` hint carries the one fill of the view, in the thread hue. Before the terminal reports its background, the fill MUST use the night theme, or the soft night theme for `auto-soft`. With colour off, it MUST be bold reverse video with no colour.

A submit sends each answer. A dismiss sends `{"answers":{}}`. Each closed question writes one row with key `question:<threadId>:<turnId>:<itemId>`. A question closed by another window or by a lost connection writes no row.

A question request that the TUI accepts MUST send a desktop notification `<product>: Waiting for your answer · <title>`, when notifications are on and the window is blurred.


At expiry the card closes and the notice says `Question timed out`. The row `<header>: Timed out` is written once. Unsent typed text is lost.

The sandbox choice is an ask card with the options Accept and Dangerously accept, and no Other row.
