# TUI

The TUI is a protocol client. It MUST import only `internal/rpc` from this module.

## Entry point

`Run(ctx, conn, opts)` dials `conn` with `rpc.Dial`. The RPC client delivers notifications and server requests in arrival order. Its dispatch queue holds 256 messages. Each message goes to the Bubble Tea program as one message.

`Options` fields:

- `OnStatus func(Status)`: called after each state change and each applied notification. `Status` holds `State`, `Seq`, and `ThreadID`. `Seq` is the highest `seq` seen in `thread/status/changed` for the current thread. That `seq` is the journal sequence of the event that changed the status. `Seq` resets to 0 when the TUI's thread changes, because each thread has its own journal.
- `OnChildSpawned func(childID, cwd string)`: called once for each `dalgona/thread/childSpawned {threadId, childId, cwd}` whose `threadId` is the TUI's thread.
- `Input` and `Output` are terminal streams. Empty fields use the process terminal.
- `ThreadID`: when set, startup calls `thread/resume` and then `thread/read` with `includeTurns: true`. When empty, startup calls `thread/start`.
- `Model`, `Approval`, `Sandbox`: option values for `thread/start` and for the status row. An empty model omits `model` from `thread/start` and shows as `default`. A model chosen with `/model` replaces it for later `thread/start` calls. The approval and sandbox defaults are `auto` and `workspace-write`. Approval `yolo` MUST force sandbox `off`. `thread/start` maps approval `yolo` to `never`, `ask` to `untrusted`, and other values to `on-request`. It maps sandbox `off` to `danger-full-access` and other values to `workspace-write`.
- `Cwd`: the client working directory. Every `thread/start` MUST send it as `cwd`.
- `Width`, `Height`: the terminal size when the output is not a terminal. Both MUST be positive to apply.

Startup runs these steps in order: `initialize` with client name `dalgona`, the `initialized` notification, `dalgona/keywords/list` (once per connection), then the thread open call. `Run` returns nil when the user quits or `ctx` ends. On return, `Run` MUST close the RPC connection.

## Screen

The TUI MUST run inline. It MUST NOT use the alternate screen. A completed item MUST print once to scrollback with `tea.Println`. Completed assistant text MUST render as Markdown through Glamour.

The dock holds, from the top, the composer, an open panel, the live row, and the status row. The live row shows the state, the notice, and the newest live text, each cut to one row:

```
<model> · <approval> · <sandbox> · <thread title>
```

Sandbox `off` MUST show as `no sandbox`. The status row MUST show the sandbox that the server reports in `thread/start`. When the harness sandbox is unavailable, the server reports `dangerFullAccess`. The TUI then shows `no sandbox`, even if the local option is `workspace-write`. A non-empty follow-up queue appends ` · <n> follow-up queued`.

The composer shows `› `, the typed text, and the cursor `▏`. When the composer is empty, the cursor comes first and the placeholder follows it in faint text. The placeholder never glows.

States are `idle`, `running`, `awaiting approval`, `interrupted`, `failed`, and `disconnected`. Each state MUST appear as text. Color MUST NOT be the only signal. Live text MUST be cut to one row, and a cut text MUST end with `...`. At 40 columns no line may exceed 39 cells.

A thread status object maps to a state. `active` with the flag `waitingOnApproval` gives `awaiting approval`. Any other `active` gives `running`. `systemError` gives `failed`. `idle` and `notLoaded` give `idle`. A turn status maps as follows: `inProgress` gives `running`, `interrupted` gives `interrupted`, `failed` gives `failed`, and `completed` gives `idle`. The `/resume` selector shows each thread's mapped state.

The TUI follows one thread. The first thread it sees, from a response or a notification, becomes the TUI's thread. `/new`, `/resume`, and `/fork` replace it. Notifications whose `threadId` names another thread, such as a spawned child, MUST NOT change the title, state, live items, or thread. When a `/new`, `/resume`, or `/fork` load applies, the TUI MUST keep the follow-up queue. It MUST show `<n> follow-ups moved to <title>; press Enter on an empty prompt to send the next` when that load applies and the queue holds at least one follow-up.

When a read from `conn` fails, the TUI MUST enter `disconnected` and show `connection lost: <cause>; restart the TUI, then /resume <thread>`. A later notification MUST NOT leave `disconnected`.

## Layout

The dock MUST be the last rows of the screen. From the top, it holds the composer, an open panel, the live row, and the status row.

The dock MUST keep its height. Only a key press, a paste, the opening or closing of a panel, or a window resize MAY change it. A failed send or steer that puts its text back in the composer MAY also change it.

The dock MUST have at most the window height minus one rows. The client MUST show no dock row in a window of fewer than two rows.

The composer MUST show at most six rows, or one row while a panel is open. A panel MUST have a fixed height for its kind, capped at the window height minus four rows.

When the dock is too tall, the client MUST drop the status row first, then the live row. The input line MUST NOT drop.

Every dock row MUST be at most the window width minus one cell.

## Print

The client MUST call `tea.Println` only from `drain`, and MUST have at most one print in flight.

The client MUST start a print only after the dock height, the window size, and the alternate screen state have not changed for three frames. A change of text inside the dock MUST NOT reset that wait.

The client MUST keep the view frozen while a print is in flight.

A print MUST hold at most the window rows minus the dock rows.

The client MUST show an empty view until its first row has printed.

A failed `initialize` MUST print one row that starts with `Could not start:`.

Each thread load MUST print its own thread row, also when the same thread loads again.

With a window height of 0, quit and suspend MUST return at once and MAY drop the rows that wait.

A tool call nested in an eval call MUST print as its own row before the row of the eval call.

The client cannot see a failed write to the terminal. A write that fails is therefore not reported.

## Keys and commands

| Input | Effect |
|---|---|
| Enter, idle | `turn/start` with the composer text |
| Enter, running or awaiting approval | `turn/steer` with the composer text |
| Enter, empty composer | Starts the oldest held follow-up when no turn runs |
| Alt+Enter | Queues a follow-up, at most 32 |
| Esc | Closes a selector. Otherwise it sends `turn/interrupt` on the active turn |
| Ctrl+C twice | Quits. The first press shows a confirmation notice |
| `/model [id]` | `model/list` selector, or `config/value/write` key `model` |
| `/resume [id]` | `thread/list` selector, or `thread/resume` then `thread/read` |
| `/new` | `thread/start` |
| `/compact` | `thread/compact/start` |
| `/login <provider>` | `account/login/start`. It shows the returned URL and user code |
| `/tree` | `dalgona/thread/tree` selector. Enter calls `dalgona/thread/leaf/set` |
| `/revert` | Active-path turns from `dalgona/thread/tree`. Enter opens a confirm panel, and its Enter calls `thread/revert`. Esc keeps the turn |
| `/fork` | `thread/fork`, then resumes the new thread |
| `/advisor` | `dalgona/advisor/status`, then `dalgona/advisor/set` with `enabled` true when no advisor is active and false otherwise. The notice says `advisor on` or `advisor off` |
| `/advisor status` | `dalgona/advisor/status`. It writes one notice line per advisor with its name, status, model, notes, and token usage |
| `/advisor reset` | `dalgona/advisor/reset` |
| `//text` | Sends `/text` as a prompt. A first token that holds a second `/`, such as a path, sends the whole text as a prompt |

While a turn is working or waiting, `/new`, `/resume`, `/fork`, `/compact`, `/tree`, and `/revert` MUST NOT send a request. The TUI MUST show `/<name> is unavailable while a turn is <word>; wait for it to finish or interrupt it first`, where `<word>` is `working`, `waiting for approval`, or `waiting for an answer`.

`turn/steer` sends `expectedTurnId` set to the active turn.

When the queue holds 32 follow-ups, Alt+Enter MUST show `follow-up queue is full (32); wait for the active turn or press Esc`. After a follow-up is queued, the notice MUST show `queued follow-up <n> of 32`. An unknown slash command MUST stay in the editor and MUST show `unknown command /<name>; start with // to send it as a prompt`. After `/advisor reset`, the notice MUST show `advisor reset; it reviews the current branch at the next turn`.

A follow-up starts after `turn/completed` only when the turn ends idle. After an interrupted or failed turn, the queue MUST stay held and the notice MUST say how to release it. After a revert, a leaf change, or `thread/reverted`, the TUI MUST show `files changed by later turns are not reverted`.

## Approvals

`item/commandExecution/requestApproval` and `item/fileChange/requestApproval` open the approval dialog. Requests queue in arrival order, and the dialog shows the oldest one. The dialog shows:

- `approval required`, followed by ` (1 of <n>)` when more requests wait.
- `thread <id>` when the request comes from another thread.
- The command when present.
- `reason: <reason>` when the request has a reason.
- The labels `[y] accept`, `[a] accept for session`, `[n] decline`, and `[esc] cancel and interrupt`. They share one line when it fits the width. Otherwise each label gets its own line.

The keys answer `accept`, `acceptForSession`, and `decline`. Esc answers `cancel` and interrupts the turn. Each request MUST block until a key answers it, the TUI exits, or the RPC context ends. A new request MUST NOT replace one that waits. Other server requests MUST fail with an error.

A server request whose parameters do not decode MUST receive an error answer. The TUI MUST show `approval request failed: <cause>; reconnect and retry the turn`.

## Notifications

The TUI applies these notifications: `thread/started`, `thread/status/changed`, `thread/name/updated` (`threadName`), `thread/reverted`, `turn/started`, `turn/completed`, `turn/plan/updated`, `item/started`, `item/completed`, the three delta notifications, `error`, `dalgona/thread/childSpawned`, and `dalgona/advisor/note`. It ignores `thread/tokenUsage/updated`, `serverRequest/resolved`, and unknown methods. The TUI MUST ignore a `thread/status/changed` whose `seq` is lower than the last seen `seq`. `turn/started` clears the notice line.

`turn/plan/updated` shows the plan in the notice line. The first line is `plan`, then one line per step: `[done] <step>`, `[in progress] <step>`, or `[pending] <step>`.

Deltas append to the live item with the same id. `item/started` and `item/completed` MUST keep text and output already streamed.

Decoding works as follows:

- The three delta notifications are the hot path. They are read with `encoding/json/jsontext` tokens and unquoted into one reused buffer. The text is then appended to per-item byte buffers.
- Other notifications and responses decode through the `internal/rpc` types with `encoding/json/v2`.
- Unknown fields MUST be ignored.

Settled status labels show `inProgress` as `running`. Other statuses show as sent.

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

## Untrusted text

The client MUST pass every server, model, paste, and command text through one sanitizer before it reaches the screen.
The sanitizer MUST remove escape sequences and control bytes, except newline and tab.
It MUST also remove DEL, C1 controls, bidirectional controls, and private-use runes from U+F0000 to U+FFFFD.
The sanitizer MUST replace each byte that is not valid UTF-8 with U+FFFD. A raw byte from 0x80 to 0x9F MUST NOT pass as a C1 control.

- Each delta of agent message, reasoning, and command output text, as it is appended.
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
A user whose terminal draws East Asian Ambiguous characters two cells wide MUST set `RUNEWIDTH_EASTASIAN=1`.

## Keyword glow

The TUI reimplements the `internal/keywords` matcher. A keyword matches only as an exact lowercase word in prose. Fenced code, inline code, HTML comments, and XML or HTML elements with their contents are masked first.

- The previous rune MUST NOT be a letter, a digit, `_`, `.`, `/`, `\`, or `-`. The two previous runes MUST NOT be `::`.
- The next rune MUST NOT be a letter, a digit, `_`, `/`, `\`, `-`, or `(`. It MUST NOT be `.` followed by a letter, a digit, `_`, or `-`.

Each matched rune takes a color from a 14-stop gradient. Stop `i` has hue `a + (b - a) * i / 14` from the keyword's `hue: [a, b]`, saturation 90%, and lightness 62%. Rune `j` of an `n`-rune word uses stop `floor(((j / n + phase) mod 1) * 14)`. The glow adds SGR sequences only. The visible text MUST NOT change.

The composer and completed user messages glow. Completed messages use phase 0. The composer matcher runs on the rendered composer text, so a cursor inside a word hides that match. While the composer has focus and shows a match, a 100 ms tick MUST advance the phase by 1/14. When focus leaves or no match shows, the TUI MUST NOT schedule another tick.
