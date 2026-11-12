# Web UI server

## Scope

`internal/webui` serves the built web UI and relays JSON-RPC between a browser and the daemon.
The caller supplies the listen address, the token, the TLS files, a daemon dialer, and an output writer.

## Assets

The package MUST embed `internal/webui/dist` with `//go:embed all:dist`.
If the root of `dist` holds no entry other than `.gitkeep`, every path except `/rpc` MUST return status 503 with the body `web UI not built; run just web`.
Otherwise the server MUST serve the files in `dist`. `/` MUST serve `index.html`.

## Token

A loopback listen address is `localhost` or a loopback IP. Every other address, including an empty host, is a LAN address.

A LAN address MUST have a configured token of at least 32 characters. Characters are Unicode code points.
Otherwise startup MUST fail before it listens, with this exact error:

```text
web: listening on <addr> needs [web] token (32+ characters)
```

`<addr>` is the configured listen address.

A loopback address MUST use the configured token when one is set.
Otherwise it MUST use a new token for each start. The token MUST be 32 bytes from `crypto/rand`, encoded as unpadded base64url.

A configured token with any other character MUST fail startup with `web: [web] token must contain only [A-Za-z0-9_-]; '+' and '&' cannot survive the URL fragment`.

The server MUST compare SHA-256 digests of the presented token and the expected token with `crypto/subtle.ConstantTimeCompare`.

## TLS

A LAN address MUST use TLS. Without `CertFile` and `KeyFile`, startup MUST fail before it listens with `web: listening on <addr> needs cert_file and key_file for TLS`. `<addr>` is the configured listen address.

When `CertFile` and `KeyFile` are both set, the server MUST serve TLS with `tls.VersionTLS13` as the minimum version.
When only one of them is set, startup MUST fail with `web: cert_file and key_file must both be set`.
A file that does not load MUST fail startup with an error that starts with `web: load cert_file and key_file: `.

## Startup output

All output goes to the caller's writer. Each message is one line.

The server MUST write `http://<addr>/#token=<token>`, or `https://<addr>/#token=<token>` with TLS.
In both lines `<addr>` is the bound listener address. The token stays in the URL fragment, so a browser does not send it in page requests.

If the listener cannot bind, startup MUST fail with `web: listen on <addr>: <error>`.

## RPC relay

`/rpc` MUST require the query parameter `token`. A missing or wrong token MUST get status 401. The server MUST NOT dial the daemon for a rejected request.

For an accepted token, the server MUST dial the daemon with a 10 second limit before the websocket upgrade.
A dial failure MUST get status 502 and MUST write `web: dial daemon: <error>`. A dial failure MUST return the body `daemon unavailable`.
The upgrade MUST use `github.com/coder/websocket` with its default origin check.

The relay MUST NOT decode, re-encode, or change message bytes.

- Browser to daemon: each websocket text message MUST become one line: the message bytes followed by one `\n`, in one write.
- Daemon to browser: each `\n`-terminated line MUST become one websocket text message without the `\n`. The relay MUST stream a line longer than the 64 KiB read buffer as one message.
- A browser message MUST be at most 16 MiB minus 1 byte, which is the daemon line limit without the newline.
- A browser message larger than that limit MUST close the websocket.

Failure behavior:

- A binary message MUST close the websocket with status 1003 (`rpc message is not text`). The daemon MUST receive no bytes from it.
- A text message that contains `\n` MUST close the websocket with status 1007 (`rpc message contains a newline`). The daemon MUST receive no bytes from it.
- When the daemon closes, the server MUST drop an unterminated last line and close the websocket with status 1001.
- When the browser closes, the server MUST close the daemon connection.
- Any other relay failure MUST close the websocket with status 1011 and MUST write `web: rpc relay: <error>`.

## Server limits and shutdown

The HTTP server MUST set a 10 second header read timeout. The HTTP server MUST set a 30 second read timeout. The HTTP server MUST set a 60 second write timeout. The HTTP server MUST set a 120 second idle timeout. A websocket connection is not subject to these limits after the upgrade.

When the context ends, the server MUST stop accepting, end every live relay, close every daemon connection, and only then return.
A clean shutdown MUST return `nil`. Shutdown waits at most 5 seconds for open page requests.

## Browser client

The client in `web/` reads the token from the URL fragment and opens `/rpc`. It speaks the Codex v2 protocol. `web/src/lib/validation.ts` parses each payload the client reads. A parser MUST reject a payload that lacks a member that the schema in `internal/rpc/testdata/codex-schema` or `internal/rpc/testdata/dalgona-schema` requires. `web/src/lib/schema.test.ts` checks this for every Codex payload parser and for the dalgona result and notification parsers.

The page holds one `Session`, which owns the connection and the page state.

### Layout

- Above 52rem the page has two columns, the thread list and the workspace. The workspace holds the thread header with its status band, the transcript, and the composer. The transcript scrolls inside its own column.
- At 52rem or narrower the page stacks the parts and the document scrolls. At 52rem or narrower the thread list (not the sidebar) keeps a maximum height of 18rem and scrolls.
- The open thread has an inline-start bar and a strong title as well as its fill, so it stays distinct under forced colours.
- At 320px nothing MUST clip or scroll sideways. Long tokens wrap anywhere.
- The transcript follows new output only while the reader is within 32px of its end. It follows the transcript at wide widths and the window at narrow widths. Opening a thread follows its end.
- The transcript MUST show a Jump to latest button while a turn runs and the reader is away from the end. The button MUST show the count of items that arrive while the reader is away as `· N new`. Activating the button MUST scroll to the end and move focus to the last row.
- At 52rem or narrower the Jump to latest button MUST sit above the dock, whatever the dock's height. The client MUST measure the dock and publish its height to the button.
- The skip link MUST name the transcript and target `#transcript`.
- At 52rem or narrower the dock (notices, request card, composer) stays at the bottom of the viewport. Form text is at least 16px.

### Theme

The `theme` setting takes `auto`, `auto-soft`, `night`, `soft-night`, `day`, or `soft-day`. Any other value MUST read as `auto`.
`auto` MUST follow the system colour scheme and use `night` or `day`. `auto-soft` MUST follow the system colour scheme and use `soft-night` or `soft-day`.
The client MUST cache the setting under the browser key `dalgona.theme` and apply it before the first render.
Each theme MUST use only the colour roles of `DESIGN.md`. The client MUST NOT paint a gradient.
Every text role MUST meet 4.5:1 against its ground. Control edges and the focus ring MUST meet 3:1 against their ground. The ink on the point MUST meet 4.5:1.
Under forced colours, every button edge and focus ring MUST keep a visible border.
The Settings panel MUST save the choice under the setting path `theme`.

### Type

Titles and control labels MUST use Bricolage Grotesque.
Body text MUST use Hanken Grotesk, and Hangul text MUST fall back to Pretendard Variable.
Code and terminal text MUST use the reader's own monospace face.
Pretendard MUST load only the subsets that a page uses.
Prose MUST use `word-break: keep-all` and `overflow-wrap: anywhere`.
Elapsed time, counts, and percentages MUST use tabular figures.
Nerd Font glyphs MUST come from the `Symbols Nerd Subset` font that `web/src/lib/theme/fonts.css` declares.
The licence texts of the fonts and the icon subset MUST ship in `web/static/licenses`.

### Thread header and status band

The thread header shows the open thread title and its state tag. The state tag is `Working`, `Waiting` while the open thread waits for the user, `Done`, `Stopped`, or `Failed`. The tag shows `Ready` when the thread is idle, and `Offline` when the connection is down.
A `turn/started` notification for a turn already shown as `Done`, `Stopped`, or `Failed` MUST NOT replace that status or clear its error.

The client MUST keep the highest applied `seq` for each thread. It MUST ignore a `thread/status/changed` whose `seq` is lower, as `docs/spec/protocol.md` requires.

The status band sits at the end of the header. It shows these items in this order: model, effort, approval, sandbox, folder, queue, context, and mark.
The client MUST NOT render an item whose id is in `statusHide`. The client MUST NOT hide `model`.

- Model shows the model name. Activating it MUST open the model panel.
- Effort shows one glyph by rank among the model's efforts. It MUST NOT show when the model has fewer than two efforts.
- The effective effort is the thread effort, or the model default when the thread sends none.
- Approval and sandbox show one glyph each. Sandbox `off` shows the none glyph.
- Folder shows one glyph, and its title is the folder name.
- Queue shows one glyph and the count, only when a follow-up is queued.
- Context shows a moon gauge and the percent of the context window in use. It MUST NOT show when the window is unknown. It MUST NOT show token counts.
- Mark shows the small candy mark.

Each icon MUST carry an `aria-label` that states its value in words. The browser draws Nerd glyphs from its own subset font, and it ignores the `icons` setting.

The band MUST keep a details list under `Status` that holds every shown item as text. The list MUST appear when `statusHide` names an item, and when the window is 40rem or narrower.

At 40rem or narrower the band is one row under the title. Below a band width of 30rem the folder item MUST drop. Below 26rem the queue item MUST drop. Below 16rem the effort item MUST drop. Below 12rem the mark item MUST drop. Model, context, approval, and sandbox MUST NOT drop.

The document title MUST be `dalgona · <title> · <state>`, with the same state word as the header tag.


### Transcript

- The transcript is not a live region. One status region announces turn changes and waiting requests.
- While a transcript loads, the status region announces `Loading transcript…`.
- Item statuses read `Working`, `Done`, `Failed`, `Declined`, or `Waiting`. Other statuses show their raw value.
- Unknown item kinds show their type in plain words, such as `Web search`.
- A `dynamicToolCall` item with tool `advisor` shows as a note card: `advisor · <severity>: <note>`. The note is the text of `contentItems`. The severity is always text. The card's inline-start border marks the severity as a second cue.
- Other dynamic tools show their `contentItems` text as output.
- `turn/plan/updated` shows the latest plan of the current turn as a checklist. Each step has a text status: `Done`, `Working`, or `Pending`. The step in progress carries `aria-current="step"`. A new turn clears the plan.
- The reasoning row reads `Thinking`. At `high`, it is open while the turn runs. At `medium` and `low`, it is one line (see Verbosity).
- Each command row MUST read `$ <command>`, show its state word, and show `exit <code>` and its duration when the server reports them.
- Each nested call MUST be its own row, placed directly after its `eval` row.
- A user message MUST underline each keyword that the server reports, and MUST list those keywords on a `Keywords:` line.
- The composer MUST list the keywords of the draft as words on a `Keywords:` line.
- A subagent of the open thread MUST appear in a subagent block with an `Open` button.
- A failed turn ends with `The turn failed: <message>. Send your message again to retry, or start a new thread.`
- A turn that is retrying after an error shows `Retrying after an error: <message>`.
- An interrupted turn shows `Turn interrupted. Send a message to continue.`
- Each file change MUST show one diff card with its path and its kind word.
- A diff card MUST show its line counts as text when the server sends `additions`.
- The kind word MUST read `Added`, `Changed`, or `Deleted`.
- A diff card MUST mark each added line and each removed line with its own element, and MUST keep the prefix character.
- A diff card MUST follow the line limit and hidden-line note of its verbosity level, as Verbosity states.
- A diff card MUST show `Diff not shown.` when the server omits the diff.
- Rows and diffs MUST be chosen by item kind, not by tool name.

Each file change MUST show its edit review as one line under its diff cards, for every verdict. A review line MUST show the summary text that the server sends, such as `No problems found`. The turn review MUST show as one line after the last row, and MUST show only for the open turn. A `flagged` review MUST use the warning text role, an `error` review MUST use bold failed text, and other reviews MUST use dim text. Review lines MUST NOT use a background. Review lines are live only, so the history MUST NOT show a verdict.

### Verbosity

- The setting `verbosity` takes `low`, `medium`, or `high`. The client MUST read it from `dalgona/settings/read`, and the default MUST be `medium`.
- At `high`, every row MUST show its full text, except a failed row, which keeps its last 10 lines.
- At `medium`, an `eval` row MUST show one line, such as `Eval · 2 lines of code · Done`.
- At `medium`, a reasoning row MUST show one line, such as `Thinking · 3 lines · ctrl+o`. While the text streams, it MUST show its last line only.
- At `medium`, a command row MUST show its last 10 lines and the total line count, such as `Showing the last 10 of 40 lines`.
- At `medium`, a diff card MUST show at most 60 lines of each file.
- At `low`, consecutive tool rows MUST join one tool group, which MUST show one summary line, such as `Ran 2 commands · called 3 tools`.
- A failed tool row MUST show its last 10 lines at every level, and it MUST end its tool group.
- The `Open full text` control of a row MUST open the review, which MUST show the full text of the row or group.
- `Ctrl+O` MUST open the review for the latest finished row when focus is in the transcript or the composer.
- The review MUST close on Escape and MUST return focus to the control that opened it.
- The review MUST load spilled command output one page at a time from `dalgona/item/output/read`.
- The rules and the summary text MUST match the Verbosity section of `docs/spec/tui.md`.

### Markdown

- The client MUST render model text with the renderer in `web/src/lib/markdown.ts`.
- The renderer MUST shift each heading down two levels, so `#` renders as `h3`.
- The renderer MUST highlight code blocks in bash, css, diff, dockerfile, go, ini, javascript, json, markdown, python, rust, sql, typescript, xml and yaml.
- The renderer MUST accept `sh` and `shell` as bash, `toml` as ini, `html` as xml, `ts` as typescript and `js` as javascript.
- The renderer MUST show raw HTML as escaped text.
- The renderer MUST keep a link only when its target starts with `http:`, `https:` or `mailto:`. Any other link MUST show its text alone.
- Each kept link MUST open in a new tab with `rel="noopener noreferrer nofollow"`.
- The renderer MUST NOT render images. An image MUST show its alternative text as `image: <text>`.
- The sanitizer MUST drop every tag and attribute that the renderer does not produce.

### Motion

A live thread shows one breathing dot and one elapsed counter. Both MUST stop under `prefers-reduced-motion: reduce`.
Under `prefers-reduced-motion: reduce` the counter MUST NOT tick on a timer. It MUST update only when the transcript changes.
The elapsed counter MUST read in the duration form of `web/src/lib/format.ts`: `0 ms`, `1s`, `1m 00s`, and `1h 00m`.
A thread that waits for a user answer or an approval MUST show `Waiting`.
During a rate-limit wait the tag MUST show `rate limited · retrying in <m>m <ss>s · esc stops` in place of the counter until `dalgona/retry/wait` reports zero.
A finished row MUST NOT tick.

### Approvals

The client records subagent threads from `dalgona/thread/childSpawned`. An approval from a subagent of the open thread MUST appear in the approval card of the open thread. The answer MUST carry the subagent request id. An approval from any other thread MUST count as waiting on another thread. The client MUST keep subagent links per parent thread id when it rebuilds the transcript from `thread/resume` or `thread/read`. It MUST NOT apply links from another thread.

The approval card MUST show `Approval required`, a title, the request details, and the decision actions. The title MUST read `Run this command?` for a command and `Apply these changes?` for a file change. The kicker MUST add `· <n> of <total>` when more than one approval is open.

The request details MUST scroll inside their own region. The decision actions MUST stay outside that region. The card MUST offer `Accept`, `Accept for session`, `Decline`, and `Cancel and interrupt`.

While the card has focus, `y` MUST accept, `a` MUST accept for the session, `n` MUST decline, and Escape MUST cancel and interrupt the turn. Escape MUST answer the card at once, and the two-press guard of the composer MUST NOT apply to it.

The card MUST move focus to its title when it appears. When the card closes and no other control has focus, focus MUST move to the message field.

Requests from other threads appear as one notice inside the workspace with an Open thread action.

A file-change approval shows the diffs carried in the request.

The dock shows one request at a time, the oldest of the open thread. Approvals, questions and the sandbox choice share that order.

### Questions

The question card MUST show `Question required`, the header and the question of each question, and its options. The kicker MUST add `· <n> of <total>` when more than one request is open.

The card MUST mark the first option `Recommended`. A question with options MUST accept one choice, or several when `multiSelect` is true. A question with `isOther` MUST also show a text field for an answer of its own.

A question without options MUST show a text field only. The preview of the chosen option MUST appear in monospace below the options.

The request details MUST scroll inside their own region. The actions MUST stay outside that region. `Submit` MUST be the card's one solid fill.

Submit MUST send each answer as its option labels in option order, then the typed text. The typed text MUST be trimmed and capped at 2000 characters. Skip MUST send every question with an empty list. Dismiss and Escape MUST send `{"answers": {}}`.

The card MUST move focus to its title when it appears. A `serverRequest/resolved` for a request this window still shows MUST show its notice in the notice stack.

### Empty states and errors

- With no open thread the transcript says `Pick a thread or start a new one`. When connected, it says that earlier threads are in the list. When not connected, it says to connect to dalgona.
- With no connection the thread list says `Threads unavailable` and does not claim that no threads exist. A thread without a title shows its preview. A thread with neither shows `Untitled thread`.
- Selecting Reconnect loads the thread list, then resumes the open thread on the new connection, so its transcript keeps streaming. The resume MUST keep queued follow-ups. Opening a different thread MUST discard them.
- A new connection MUST discard thread resumes still in flight from the previous connection.
- In the approval card, Decline and Cancel use the quiet style. The composer's `Interrupt turn` button has the danger style.

### Notices

Two regions MUST be on the page before any notice arrives.
The alert region holds errors. The status region holds warnings and information.
A notice MUST name only the controls that it shows.
Dismiss MUST remove its notice and move focus to the message field.
A notice with the same text as a notice on the page MUST replace it.
The page MUST keep at most five notices. It MUST remove the oldest first.
A failed request MUST show `<action>: <detail>.`. While connected, the notice MUST offer `Try again`, which repeats the request. Otherwise it MUST offer `Reconnect`.
A queued follow-up whose send fails MUST leave the queue. Its notice's `Try again` MUST send it once.
A dropped connection MUST show `Disconnected from dalgona.` with `Reconnect`.
A failed connect MUST show `Unable to connect to dalgona: <detail>. Check that dalgona web is still running.` with `Reconnect`.
A socket error MUST show `Connection error: <message>.` with `Reconnect`.
The header MUST show `Offline` while the connection is down.

### Notifications

The client MUST send a desktop notification only while the tab is hidden and the browser has granted permission.
A waiting request MUST send `Waiting for your reply`.
A turn that ends `completed`, `failed`, or `interrupted` MUST send `Done`, `Failed`, or `Stopped`.
Each notification MUST name its thread in the body and use the thread id as its tag.
The client MUST NOT ask for permission on load. It MUST ask only when the user chooses `Allow desktop notifications`.
The last row of the Settings panel MUST show `Desktop notifications are on`, `Desktop notifications are blocked in this browser`, or `Desktop notifications are off`.
The Settings panel row MUST show the button only while permission is undecided.
A browser without the Notification API MUST show no Settings panel row.

### Focus

A control that disappears while focused hands focus to the message field. The submit control keeps its element and changes only its label.

Start thread stays focusable and reports `aria-disabled` while a start is in flight.

### Composer

While a turn runs, Alt+Enter MUST queue the message as a follow-up. Enter MUST steer the turn instead.
The queue MUST list each follow-up by its first line, above the message field.
Edit MUST put the follow-up's text into the message field and remove the follow-up from the queue.
Remove MUST drop only the follow-up that it names.

ArrowUp MUST recall the previous message when the caret is at the start of the message field and nothing is selected.
ArrowDown MUST recall the next message when the caret is at the end or while you browse. It MUST restore the draft after the newest message.
Typing into a recalled message MUST make the edited text the draft. The caret MUST move to the start of the message field after each recall.

Text of 10 or more lines pasted into the message field MUST become a chip that shows its line count. Show MUST expand the chip text in a read-only block, and Remove MUST drop the chip.
Pasted images MUST be PNG, JPEG or GIF files of at most 5 MiB. The composer MUST keep at most 10 images. The composer MUST refuse an image with `this model does not accept images` when the selected model has no image input.

Enter MUST send the typed text, then the pasted texts, then the images, as one message. A steer or follow-up MUST keep the chips for the next send.
An empty typed text MUST NOT add a text item to the message.
A message whose input array exceeds 15 MiB of JSON MUST stay in the composer with an error, and MUST NOT be sent. The limit is measured on the input array, not on the whole turn/start request.

A message that starts with `!` MUST be a shell command, and Enter MUST run it in the thread's working directory.
The field MUST be labelled `Shell command` in shell mode.
The help line MUST read `Enter runs the command in <cwd>. It is not sent to the agent.`
The client MUST call `command/exec` with `sh -c`, or with `cmd /C` on Windows.
The client MUST time out a shell command after 120 seconds and keep at most 64 KiB of output.
A shell command MUST commit one row to the transcript and MUST NOT be journaled.
The output MUST become a chip labelled `[Shell #n exit N, L lines]`. The next message MUST send the chip's text as one more text block.
An empty shell line MUST NOT run and MUST show `Enter a command after !.`
A shell command whose call fails MUST show a failed row and MUST NOT make a chip.
A shell command with no open thread, or in a thread with no working directory, MUST show `Open a thread before running a shell command` and MUST keep the draft.
A shell command that finishes after the user opens another thread MUST NOT add its row or its chip to that thread.
Alt+Enter MUST NOT queue a message that starts with `!`. It MUST show `Shell commands run with Enter`.
The output fence MUST be longer than any run of backticks in the output, and MUST be at least three backticks.

| Key | Caret and selection | Effect |
|---|---|---|
| ArrowUp | At the start, nothing selected | Recalls the previous message you sent. |
| ArrowDown | At the end, or while browsing, with nothing selected | Recalls the next message, or restores the draft. |

While a turn runs, the first Escape MUST show `Esc again to interrupt` in the help line for two seconds.
A second Escape within those two seconds MUST interrupt the turn.
During a rate-limit wait, one Escape MUST interrupt the turn at once.
Escape while no turn runs MUST do nothing.

An `@` that starts a word MUST start a file search for the text after it, up to the next whitespace.
The file search MUST wait 120 milliseconds after the last keystroke and MUST NOT run for an empty query.
The file search MUST use the open thread's working directory as its only root, even when the thread is not listed.
With no working directory known, the file search MUST send no request and MUST show no files.
The file list MUST show up to 20 files, each with its file name and its path.
Enter or Tab MUST insert the highlighted path as `@<path>` followed by a space, and MUST NOT send the message.
A change to the text after `@` MUST clear the file list at once.
Enter or Tab MUST NOT send the message while a search for the current text is pending.
A caret move inside the same `@` word MUST NOT start another search.
Escape MUST close the file list and MUST keep the message field.

### Commands and panels

A message that starts with `/` MUST open the command list.
The command list MUST show each command whose name starts with the text after `/`. Once a space follows, the command list MUST show nothing, and Enter MUST open no panel and keep the message field.
ArrowUp and ArrowDown MUST move the highlight through the list.
Enter or Tab MUST open the panel of the highlighted command and MUST clear the message field.
Escape MUST close the command list and MUST keep the message field.
Recalling a message or editing a follow-up MUST reopen the command list when the text starts with `/`.
A message that starts with `//` MUST be sent with one slash removed.
A first word that holds a second slash, such as `/etc/hosts`, MUST be sent as text.
Submit with a command name that no command has MUST NOT send the message.
That submit MUST show `No command named /<name>. Start with // to send a message that begins with /.`

| Command | Panel | Summary |
|---|---|---|
| `/model` | model | Choose a model or preset |
| `/settings` | settings | Change theme, verbosity, approval, status items, edit review, and notifications |
| `/login` | login | Log in to a provider |
| `/resume` | resume | Open an earlier thread |
| `/new` | new | Start a new thread |
| `/compact` | compact | Compact this thread |
| `/fork` | fork | Fork this thread at a turn |
| `/tree` | tree | Move to another branch of this thread |
| `/revert` | rewind | Rewind to before a turn |
| `/advisor` | advisor | Show or change the advisor |
| `/review` | review | Review changes |
| `/help` | keys | Show keys |

Pressing `?` while no field has focus MUST open the key list.
Escape MUST close a panel and MUST return focus to the element that opened it.
The Settings panel MUST save only the settings whose values changed, in one `config/batchWrite` call.
The Settings panel MUST send the key paths `approval`, `verbosity`, `theme`, `statusHide`, and `editReview`.
An empty or unknown choice MUST NOT be saved, and the panel MUST show `Choose a <label>.` in its body.
After a save, the client MUST apply the theme at once and MUST reload the settings.
A save that changes `approval` MUST show `saved; open threads keep their approval and sandbox`.

The login panel MUST list each provider from `dalgona/account/providers/list`.
An API key MUST be sent with `account/login/start` only after the user types it.
A failed API-key login MUST keep the panel open and MUST show the error under the key field.
A browser or device-code login MUST wait for `account/login/completed` with its `loginId`.
A successful login MUST show `Signed in to <provider>`, reload the models, and close the panel.
A failed browser or device-code login MUST show `Login failed` with a Try again action.
Closing the login panel or opening another panel MUST cancel a pending browser or device-code login with `account/login/cancel`.
A start response that arrives after the cancel MUST be cancelled with the `loginId` it returns.

The model panel MUST list each visible model under `Models`, with its description.
The model panel MUST list each preset under `Presets`, as the preset, the model, and its default effort.
Choosing a preset MUST fill the model and its default effort.
The model panel MUST save the model and its effort in one `config/batchWrite` call, with the key paths `model` and `model_reasoning_effort`.
The model panel MUST NOT save a model that the list lacks, or an effort that the model does not offer.
A model panel with no models MUST show `No models. Log in to a provider to list its models.` with a Log in action.

The resume panel MUST list threads from `thread/list` with a filter field.
The fork panel MUST list the turns of the open thread and MUST send `thread/fork` with the chosen turn.
The tree panel MUST list the turns with their preview and an Active mark, and MUST send `dalgona/thread/leaf/set` for the chosen turn.
The rewind panel MUST ask for confirmation before it sends `thread/revert`.
The advisor panel MUST show each advisor and MUST offer turning the advisor on or off and resetting it.
The review panel MUST send `review/start` with the chosen target and MUST NOT send it with an empty branch, commit, or instructions.
Each user message MUST show a Rewind to here control that opens the rewind panel on that turn.
While a turn runs, `/new`, `/compact`, `/fork`, `/tree`, and `/revert` MUST NOT open a panel.
That refusal MUST show `/<name> is unavailable while a turn is working; wait for it to finish or interrupt it first` with Interrupt and Close actions.

### Accessibility

Text MUST meet a contrast ratio of 4.5 to 1 against its ground, and interface parts MUST meet 3 to 1. The contrast test in `web/src/lib/theme/contrast.test.ts` checks each theme.

Each focusable control MUST show a 2 px focus ring. The keyboard paths test in `web/src/lib/components/App.dom.test.ts` MUST pass for each path.
Each keyboard path MUST send a message, accept an approval, open and close each panel, or open the review with Ctrl+O.

Each visible control MUST be at least 24 by 24 CSS pixels. A button MUST be at least `--control-height` tall, and at least `--control-height-coarse` tall under a coarse pointer. `web/checks/target-size.js` checks these rules.

Form text MUST be at least 16 pixels. `web/checks/input-size.js` checks this rule.

Under `prefers-reduced-motion: reduce`, animations MUST stop, and the elapsed text MUST NOT change without a new item. `web/checks/reduced-motion.js` checks this rule.

Under forced colours, each state tag and the point button MUST keep a visible border. `web/checks/forced-colors.js` checks this rule.

The page MUST have one status region and one alert region. The transcript MUST NOT be a live region.
