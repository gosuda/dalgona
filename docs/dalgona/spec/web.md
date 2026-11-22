# Web UI server

## Scope

`internal/dalgona/webui` serves the built web UI and relays JSON-RPC between a browser and the daemon.
The caller supplies the listen address, the token, the TLS files, a daemon dialer, and an output writer.

## Assets

The package MUST embed `internal/dalgona/webui/dist` with `//go:embed all:dist`.
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

The client in `web/` reads the token from the URL fragment and opens `/rpc`. It speaks the Codex v2 protocol. `web/src/lib/validation.ts` parses each payload the client reads. A parser MUST reject a payload that lacks a member that the schema in `internal/dal/rpc/testdata/codex-schema` or `internal/dal/rpc/testdata/dalgona-schema` requires. `web/src/lib/schema.test.ts` checks this for every Codex payload parser and for the dalgona result and notification parsers.

The page holds one `Session`, which owns the connection and the page state.

Text input MUST follow [input-text.md](../../dal/spec/input-text.md).
That contract defines required adapter changes. These requirements do not assert that the existing browser client already conforms.

### Layout

- Above 52rem the page has two columns, the thread list and the workspace. The workspace holds the thread header with its status band, the transcript, and the composer. The transcript scrolls inside its own column.
- At 52rem or narrower the page stacks the parts and the document scrolls. At 52rem or narrower the thread list (not the sidebar) keeps a maximum height of 18rem and scrolls.
- At 52rem or narrower the transcript MUST NOT be a scroll container. Wheel input MUST scroll the document.
- The open thread has an inline-start bar and a strong title as well as its fill, so it stays distinct under forced colours.
- At 320px nothing MUST clip or scroll sideways. Long tokens wrap anywhere.
- The transcript follows new output only while the reader is within 32px of its end. Opening a thread follows its end. While no thread is open the client MUST NOT scroll to follow the transcript. While the transcript follows, output that arrived after the client's last scroll or scroll event MUST NOT count as the reader leaving the end.
- The client MUST observe the actual scroll container: the transcript when it overflows, else its nearest scrolling ancestor, else the document. The client MUST resolve it on each scroll, so content that overflows only after it arrives is observed. Streaming MUST preserve the reader's position after scrolling away.
- While a thread is open, the transcript MUST show Jump to latest whenever the reader is over 32px from the end. Whether a turn runs MUST NOT change that. The button MUST show appended items as `· N new`. Activating it MUST scroll to the end and focus the last row without changing that scroll position. Showing the button MUST NOT move focus.
- At 52rem or narrower the Jump to latest button MUST sit above the dock, whatever the dock's height. The client MUST measure the dock and publish its height to the button.
- The skip link MUST name the transcript and target `#transcript`.
- At 52rem or narrower the dock (notices, request card, composer) MUST stay at the bottom of the viewport at every scroll position. At the end of the document the dock MUST NOT cover the last transcript row. The dock MUST span the width of the viewport. While no request is open, the dock MUST leave at least 6rem of the viewport above it. The dock MUST scroll inside itself when its content is taller than that. Form text is at least 16px.

### Theme

The `theme` setting takes `auto`, `auto-soft`, `night`, `soft-night`, `day`, or `soft-day`. Any other value MUST read as `auto`.
`auto` MUST follow the system colour scheme and use `night` or `day`. `auto-soft` MUST follow the system colour scheme and use `soft-night` or `soft-day`.
The client MUST cache the setting under the browser key `dalgona.theme` and apply it before the first render.
Each theme MUST use only the colour roles of `DESIGN.md`. The client MUST NOT paint a gradient.
Every text role MUST meet 4.5:1 against its ground. Control edges and the focus ring MUST meet 3:1 against their ground. The ink on the point MUST meet 4.5:1.
Under forced colours, every button edge and focus ring MUST keep a visible border.
Outside forced colours, a quiet button MUST draw no fill at rest. A quiet button that is not destructive MUST draw no edge. The label of that button MUST use `dim`. A destructive quiet button MUST draw its edge and its label in `failed-text`. A disabled quiet button MUST draw the `tone` fill, the `line` edge and the `dim` label. A quiet button that is not destructive MUST take the padding 0 12 px of `DESIGN.md`. Each button MUST take the type of the Button row of `DESIGN.md`. That type is weight `--weight-control`, size `--size-label`, line height `--leading-tight`, inline flex, centred, and no wrap. Thread rows, thread choices of the Resume panel, the model button of the status band, and request card actions MUST NOT take that type. Thread rows and thread choices are list rows whose labels wrap. The model button is a band item that the Status band section of `DESIGN.md` cuts with `…`. Request card actions are the decision actions of the card, whose labels wrap in its grid. `web/checks/quiet-buttons.js` checks these rules for each quiet button in view, and the Button type for each other button in view.
Each component value MUST use a named token of `DESIGN.md`. A token MUST NOT hold a value absent from the `DESIGN.md` component tables. The contract test in `web/src/lib/theme/design-contract.test.ts` checks each `DESIGN.md` citation against `web/src/lib/theme/scale.ts`.
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
Each state tag MUST follow the Status tag, Live tag, and Failed tag rows of `DESIGN.md`. It MUST take the colour that the State words table of `DESIGN.md` gives its word. `web/checks/state-tags.js` checks these rules for each tag in view.
The visible thread title MUST use at most two lines.
The header MUST retain the full title as text and as a `title` attribute.
The client MUST map thread status `notLoaded` to idle and display `Ready` when no completed turn is known.
A `turn/started` notification for a turn already shown as `Done`, `Stopped`, or `Failed` MUST NOT replace that status or clear its error.

The client MUST keep the highest applied `seq` for each thread. It MUST ignore a `thread/status/changed` whose `seq` is lower, as `docs/dal/spec/protocol.md` requires.

The status band sits at the end of the header. It shows these items in this order: model, effort, approval, sandbox, folder, queue, context, and mark.
The client MUST NOT render an item whose id is in `statusHide`. The client MUST NOT hide `model`.

- Model shows the model name. Activating it MUST open the model panel.
- Effort shows one glyph by rank among the model's efforts. It MUST NOT show when the model has fewer than two efforts.
- The effective effort is the thread effort, or the model default when the thread sends none.
- Approval and sandbox show one glyph each. Sandbox `off` shows the none glyph.
- Folder shows one glyph, and its title is the folder name.
- Queue shows one glyph and the count, only when a follow-up is queued.
- Context shows a moon gauge and the percent of the context window in use. It MUST NOT show when the window is unknown. It MUST NOT show token counts. The percent MUST use the temporary context estimate while one is present, even at zero, until a provider usage report supersedes it.
- Mark shows the small candy mark.

Each icon MUST carry an `aria-label` that states its value in words. The browser draws Nerd glyphs from its own subset font, and it ignores the `icons` setting.

The band MUST keep a details list under `Status` that holds every shown item as text. The list MUST appear when `statusHide` names an item, and when the window is 40rem or narrower.

Above 40rem the band is three rows at the end of the header. The first row holds `model`. The second row holds `effort`, `approval`, `sandbox`, `folder`, and `queue`. The third row holds `context` and `mark`. Each row MUST end at the right edge of the band.

At 40rem or narrower the band is one row under the title. Below a band width of 30rem the folder item MUST drop. Below 26rem the queue item MUST drop. Below 16rem the effort item MUST drop. Below 12rem the mark item MUST drop. Model, context, approval, and sandbox MUST NOT drop.

The document title MUST be `dalgona · <title> · <state>`, with the same state word as the header tag.


### Transcript

- The transcript is not a live region. One status region announces turn changes and waiting requests.
- While a transcript loads, the status region announces `Loading transcript…`.
- The status region MUST announce a non-empty `reviewNote` for an edit-review notice.
- Item statuses read `Working`, `Done`, `Failed`, `Declined`, or `Waiting`. Other statuses show their raw value.
- Unknown item kinds show their type in plain words, such as `Web search`.
- A `dynamicToolCall` item with tool `advisor` shows as a note card: `advisor · <severity>: <note>`. The note is the text of `contentItems`. The severity is always text. The card's inline-start border marks the severity as a second cue.
- Other dynamic tools show their `contentItems` text as output. An output line shaped `[<path>#<tag>]` or `[<path>@<ref>]`, where `<tag>` is a hashline tag and `<ref>` is a snapshot reference, MUST NOT show.
- `turn/plan/updated` shows the latest plan of the current turn as a checklist. Each step has a text status: `Done`, `Working`, or `Pending`. The step in progress carries `aria-current="step"`. A new turn clears the plan.
- The reasoning row reads `Thinking`. At `high`, it is open while the turn runs. At `medium` and `low`, it is one line (see Verbosity).
- Each command row MUST read `$ <command>`, show its state word, and show `exit <code>` and its duration when the server reports them.
- Each nested call MUST be its own row, placed directly after its `eval` row.
- The browser MUST identify each transcript item by its turn id and item id together.
- Repeated item ids in different turns MUST keep distinct rows. Notifications MUST update only the item in their named turn.
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
- A diff card MUST show its lines without an inner scroll container.

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
- The rules and the summary text MUST match the Verbosity section of `docs/dal/spec/tui.md`.

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
- A fenced block with info `mermaid`, `d2`, `nomnoml` or `svg` MUST render to a sanitized SVG figure.
- A failed diagram render MUST mark the block `diagram-error` and MUST keep the source text.
- Diagram SVG MUST drop `style` elements and attributes and every element that loads a resource.

### Motion

A live thread shows one breathing dot and one elapsed counter. Both MUST stop under `prefers-reduced-motion: reduce`.
Under `prefers-reduced-motion: reduce` the counter MUST NOT tick on a timer. It MUST keep its value until the transcript changes.
A turn that starts under that preference MUST show a fresh counter.
The counter MUST follow a change of that preference while the page is open. A change to reduce MUST NOT change the counter. When motion is allowed again, it MUST show the real elapsed time at once and tick every second.
The elapsed counter MUST read in the duration form of `web/src/lib/format.ts`: `0 ms`, `1s`, `1m 00s`, and `1h 00m`.
The counter MUST start from the turn start that the server reports, in `turn/started` or in a resumed turn. A `turn/start` response without a start for the open turn MUST NOT clear the start. A different turn MUST NOT inherit the start of an earlier turn. The counter MUST stay empty until the server reports a start.
A thread that waits for a user answer or an approval MUST show `Waiting`.
During a rate-limit wait the tag MUST show `rate limited · retrying in <m>m <ss>s · esc stops` in place of the counter until `dalgona/retry/wait` reports zero.
A finished row MUST NOT tick.

### Approvals

The client records subagent threads from `dalgona/thread/childSpawned`. An approval from a subagent of the open thread MUST appear in the approval card of the open thread. The answer MUST carry the subagent request id. An approval from any other thread MUST count as waiting on another thread. The client MUST keep subagent links per parent thread id when it rebuilds the transcript from `thread/resume` or `thread/read`. It MUST NOT apply links from another thread.

The approval card MUST show `Approval required`, a title, the request details, and the decision actions. The title MUST read `Run this command?` for a command and `Apply these changes?` for a file change. The kicker MUST add `· <n> of <total>` when more than one approval is open.

The request details MUST scroll inside their own region. The decision actions MUST stay outside that region. The card MUST offer `Accept`, `Accept for session`, `Decline`, and `Cancel and interrupt`.
ArrowUp and ArrowDown MUST scroll the approval details by 48 px. PageUp and PageDown MUST scroll the approval details by the height of the details minus 48 px. Each PageUp and PageDown scroll MUST move at least 48 px.
The dock MUST fit within the viewport. Approval details MUST shrink with the viewport and scroll without hiding the card header or actions.
Approval details MUST retain a readable scroll region. The card header, actions, and composer MUST NOT overlap at any viewport size.
The request card MUST NOT shrink below the height of its header, details, and actions.
While a request is open, the composer MUST give up height before the card does, down to its label and one field row.
When the card, the notices, and the composer at that height are taller than the viewport, the dock MUST scroll inside itself.
While the dock scrolls, the sticky actions MAY cover part of the details until the dock reaches its end.
While a request is open, the composer MUST keep its label, field, and actions reachable by scrolling the composer or the dock.

While the card has focus, `y` MUST accept, `a` MUST accept for the session, `n` MUST decline, and Escape MUST cancel and interrupt the turn. Escape MUST answer the card at once, and the two-press guard of the composer MUST NOT apply to it.
These shortcuts MUST NOT run while a text field or IME composition owns the event.

The card MUST move focus to its title when it appears. When the card closes and no other control has focus, focus MUST move to the message field.

Requests from other threads appear as one notice inside the workspace with an Open thread action.

A file-change approval shows the diffs carried in the request.

The dock shows one request at a time, the oldest of the open thread. Approvals, questions and the sandbox choice share that order.

### Questions

The question card MUST show `Question required`, the header and the question of each question, and its options. The kicker MUST add `· <n> of <total>` when more than one request is open.

The card MUST mark the first option `Recommended`. A question with options MUST accept one choice, or several when `multiSelect` is true. A question with `isOther` MUST also show a text field for an answer of its own.

A question without options MUST show a text field only. The preview of the chosen option MUST appear in monospace below the options.

The request details MUST scroll inside their own region. The actions MUST stay outside that region. `Submit` MUST be the card's one solid fill.
While the title or an action of the question card has focus, ArrowUp and ArrowDown MUST scroll the question details by 48 px.
PageUp and PageDown MUST scroll the question details by the height of the details minus 48 px. Each scroll MUST move at least 48 px.
Home and End MUST scroll the question details to their top and to their end. The details region MUST NOT take a tab stop.

Submit MUST send each answer as its original option labels in option order, then the accepted typed text.
Display escaping MUST NOT change an option's original label or identifier.
Typed answers MUST obey the shared 2000 Unicode-scalar limit. They MUST NOT be trimmed or silently truncated.
An over-limit candidate MUST remain recoverable, with a field error, and MUST NOT submit or advance.
HTML `maxlength` MUST NOT enforce this quota because it counts UTF-16 code units.
Skip MUST send every question with an empty list. Dismiss and Escape MUST send `{"answers": {}}`.
After submitting, the card MUST retain its answers until `serverRequest/resolved` closes the request.
A validation warning for that request MUST restore editing and keep its original request ID, deadline, and complete answer text.
The card MUST NOT treat local submission as proof that the daemon accepted an answer.

The card MUST move focus to its title when it appears. A `serverRequest/resolved` for a request this window still shows MUST show its notice in the notice stack.

A `serverRequest/resolved` with `timedOut` closes the card and shows the notice `Question timed out`. The transcript row of the question item shows `Timed out`.

### Empty states and errors

- With no open thread the transcript says `Pick a thread or start a new one`. When connected, it says that earlier threads are in the list. When not connected, it says to connect to dalgona.
- On a fresh load the client MUST NOT open a thread by itself.
- With no connection the thread list says `Threads unavailable` and does not claim that no threads exist. A thread without a title shows its preview. A thread with neither shows `Untitled thread`.
- The `thread/list` request MUST carry `limit` and `cursor` only. It MUST NOT carry `sortKey` or `sortDirection`.
- The sidebar and resume panel MUST request 100 threads at a time. If a page has a next cursor, `Load more threads` MUST use that cursor and append unseen threads in response order. Loading a page MUST keep the current thread open. A failed request MUST keep the listed threads and offer a retry.
- When pages overlap, the client MUST check earlier pages for threads that moved before the requested offset.
- On a final page without overlap, the client MUST check the first page. If its thread IDs and update times match in order, the client MUST stop there. If they changed, the client MUST check the other earlier pages.
- A retry MUST request the failed page with the same cursor. The sidebar MUST remove the error notice when it retries that page.
- Selecting Reconnect loads the thread list, then resumes the open thread on the new connection, so its transcript keeps streaming. The resume MUST keep queued follow-ups. Opening a different thread MUST discard them.
- A `thread/resume` that fails with `-32602` (unknown thread) MUST clear the open thread and its transcript.
- A new connection MUST discard thread resumes still in flight from the previous connection.
- In the approval card, Decline and Cancel use the quiet style. The composer's `Interrupt` button has the danger style.

### Notices

Two regions MUST be on the page before any notice arrives.
The alert region holds errors. The status region holds warnings and information.
A notice MUST name only the controls that it shows.
Dismiss MUST remove its notice and move focus to the message field.
A notice with the same text as a notice on the page MUST replace it.
The page MUST keep at most five notices. It MUST remove the oldest first.
A failed request MUST show `<action>: <detail>.`. While connected, the notice MUST offer `Try again`, which repeats the request. Otherwise it MUST offer `Reconnect`.
A queued follow-up whose send fails MUST leave the queue. Its notice's `Try again` MUST send it once.
A dropped connection MUST show `Disconnected from dalgona.` with `Reconnect` once no attempts remain.
A failed first or manual connect MUST show `Unable to connect to dalgona: <detail>. Check that dalgona web is still running.` with `Reconnect`.
A socket error MUST show `Connection error: <message>.` with `Reconnect`.
The header MUST show `Offline` while the connection is down.
A connect that starts from a down connection MUST NOT show `connecting`. The header MUST show `Offline` until that connect reports `connected`.
For a local daemon, `dalgona web` MUST permit auto-start until its first successful daemon connection.
After that connection, each relay connection MUST attach to an existing daemon without auto-start.
This rule applies to all browser windows served by that `dalgona web` process.
If the daemon stops, start it separately before reconnecting. Remote connections keep their configured bridge behavior.
After a drop, the client MUST start connects by itself. Each connect that the client starts is an attempt.
The client MUST start at most 3 attempts in a row. They start 1 second, 2 seconds, and 4 seconds after the failure before them.
Each attempt MUST fail after 30 seconds without an open socket.
A manual connect is a connect that the user starts with `Reconnect`.
A failure of a manual connect or of the first connect of a page load MUST NOT start attempts.
While attempts remain, the notice MUST read `Reconnecting to dalgona (attempt <n> of 3).` with `Reconnect`.
A drop or a failed attempt MUST remove the error notices. The header MUST keep `Offline` until an attempt connects.
The client MUST ignore notifications, requests, and errors that arrive from the connection that it dropped.
The attempt count MUST reset when a connection stays up for 20 seconds. A drop MUST cancel that wait.
Selecting `Reconnect` MUST cancel the next attempt and connect at once.
An attempt MUST resume the open thread as `Reconnect` does. It MUST NOT send a turn again, because the server owns running turns.

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
Load more threads stays focusable and reports `aria-disabled` while a page loads.
Each panel action and the API key field MUST stay focusable while the request they started is in flight. The action MUST report `aria-disabled` while that request runs and MUST take the disabled styling while it does. Activating it again MUST NOT send a second request.
When the focused button disappears, the sidebar MUST move focus to the message field, or to the transcript if the field is disabled.
When the focused button disappears in the resume panel, the panel MUST move focus to its filter.

Only one control in one view MUST carry the point fill. The holder is the primary action of an open request, or the open panel.
With no request or panel open, it is the live state tag of a running open thread.
With no running turn, it is `Send` in the message field of an idle open thread. With no open thread, `Send` MUST NOT carry the point fill.
On the empty view, it is the main `Start thread` button. The sidebar `Start thread` button MUST NOT carry the point fill.

The open thread view MUST carry the hue of its thread.
A view with no open thread MUST carry no hue.

### Text input

The shared input contract applies to every editable field, including prompts, question answers, search, review targets, credentials, titles, and paths.
Authored text MUST retain its accepted scalar sequence. Whitespace-only eligibility checks MUST NOT trim nonblank text before storage or submission.
Derived search keys and display escapes MUST remain separate from canonical text and selected server identities.
Multiline adapters MUST fold CRLF and lone CR to LF once under [input-text.md](../../dal/spec/input-text.md).
A multiline paste into a one-line field MUST fail atomically with a field error and preserve the old value and selection.
The rejected candidate MUST remain recoverable without inserting a prefix.

The browser owns native editing, selection, composition, and undo within each field.
Native editing MUST pass the shared grapheme corpus. A failing browser MUST receive a tested adapter correction before release.
Application replacements and range conversions MUST preserve the extended grapheme boundaries required by the shared contract.
DOM selection offsets and JavaScript string indices are UTF-16 code units. They MUST NOT be used as UTF-8 byte or grapheme counts.
Conversion to protocol byte offsets MUST validate the original scalar sequence and both range endpoints.
An invalid or stale keyword span MUST leave the original text visible without marks, as the shared offset contract requires.
`TextEncoder` replacement of a lone surrogate MUST NOT turn an invalid candidate into accepted input.
Transient unpaired surrogates MAY remain in adapter-owned composition state. They MUST NOT enter requests, history, storage, search, or keyword calls.

Composition ownership MUST cover `compositionstart`, updates, the final text commit, and the key event that ends that composition.
`isComposing`, `Process`, and legacy `229` signals supplement that ownership. A single false flag MUST NOT release a committing key to application shortcuts.
Enter, Escape, arrows, Tab, approval keys, history, palettes, steer, and follow-up shortcuts MUST defer while composition owns them.
Composition cancellation MUST restore the prior committed text and selection without sending, interrupting, dismissing, or accepting a request.
An oversized composition commit MUST remain recoverable and unsent. It MUST NOT become a truncated prefix or a partial cluster.
Synthetic event tests MUST be supplemented by the real browser and IME gates in [input-text.md](../../dal/spec/input-text.md).

A completion MUST bind its field, draft revision, query, and selection range before replacement.
An asynchronous result MUST be discarded when any binding changes. It MUST NOT edit active composition or split a grapheme.
An accepted replacement MUST form one undoable edit and preserve the chosen path's original scalar sequence.
Draft recovery, question review, chip expansion, history, and follow-up editing MUST preserve the same committed text.
Each invalid field MUST expose its error through `aria-describedby` and an alert without moving focus away from the recoverable draft.

### Composer

While a turn runs, Alt+Enter MUST queue the message as a follow-up. Enter MUST steer the turn instead.
The queue MUST list each follow-up by its first line, above the message field.
Edit MUST put the follow-up's text into the message field and remove the follow-up from the queue.
Remove MUST drop only the follow-up that it names.

ArrowUp MUST recall the previous message when the caret is at the start of the message field and nothing is selected.
ArrowDown MUST recall the next message when the caret is at the end or while you browse. It MUST restore the draft after the newest message.
Typing into a recalled message MUST make the edited text the draft. The caret MUST move to the start of the message field after each recall.

Text of 10 or more lines pasted into the message field MUST become a chip that shows its line count. Show MUST expand the chip text in a read-only block, and Remove MUST drop the chip.
Line counting MUST occur after the shared line-ending conversion. The chip MUST preserve the final LF and every other accepted scalar.
Chip identities MUST remain separate from Unicode text. Expanding a chip MUST recover its exact accepted text.
Pasted images MUST be PNG, JPEG or GIF files of at most 5 MiB. The composer MUST keep at most 10 images. The composer MUST refuse an image with `this model does not accept images` when the selected model has no image input.

Enter MUST send the typed text, then the pasted texts, then the images, as one message. A steer or follow-up MUST keep the chips for the next send.
An empty typed text MUST NOT add a text item to the message.
A message whose input array exceeds 15 MiB of JSON MUST stay in the composer with an error, and MUST NOT be sent. The limit is measured on the input array, not on the whole turn/start request.
The client MUST count UTF-8 bytes of the complete serialized input array, including expanded chips, JSON escapes, and image items.
It MUST NOT estimate that limit with JavaScript string length. Rejection MUST retain text, selection, chips, and the unsent candidate.
The client MUST remove one slash from a message that starts with `//` on the send, steer and queue paths.

A message that starts with `!` MUST be a shell command, and Enter MUST run it in the thread's working directory.
The field MUST be labelled `Shell command` in shell mode.
The help line MUST read `Enter runs the command in <cwd>. It is not sent to the agent.`
The client MUST call `command/exec` with `sh -c`, or with `cmd /C` on Windows.
The client MUST time out a shell command after 120 seconds and keep at most 64 KiB of output.
A shell command MUST commit one row to the transcript and MUST NOT be journaled.
The output MUST become a chip labelled `[Shell #n exit N, L lines]`. The next message MUST send the chip's text as one more text block.
An empty shell line MUST NOT run. It MUST show `Enter a command after !.`
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
The client MUST take that directory from the required top-level `cwd` of the `thread/start` or `thread/resume` response.
With no working directory known, the file search MUST send no request and MUST show no files.
The file list MUST show up to 20 files, each with its file name and its path.
Arrow keys move only through the shown rows, at most 20.
Enter or Tab MUST insert the highlighted path as `@<path>` followed by a space, and MUST NOT send the message.
A change to the text after `@` or to the search root MUST clear the file list at once.
A reply for an earlier search MUST NOT change the list.
When the root changes and the mention still has a query, the client MUST search again under the new root.
Enter or Tab MUST NOT send the message while a search for the current text is pending.
A caret move inside the same `@` word MUST NOT start another search.
Escape MUST close the file list and MUST keep the message field.
A change to a different search root MUST clear that dismissal.

### Commands and panels

Only literal ASCII `/`, `!`, and `@` MAY trigger their command grammar.
Unicode lookalikes MUST remain text. Parsing MUST NOT normalize the draft or trim argument payloads.
Removing the explicit `//` escape or `!` shell marker MUST preserve the remaining text exactly.

A message that starts with `/` MUST open the command list.
The command list MUST show each command whose name starts with the text after `/`.
An ASCII space in a slash command MUST hide the command list.
The composer MUST NOT offer file completion for a command argument.
Enter on a command with an ASCII space MUST keep the draft.
Enter in this state MUST NOT open a panel, send the draft, or show an unknown-command error.
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
When the opener of a panel is gone, disabled, or the page body, closing the panel MUST move focus to the message field.
While the message field is disabled, that focus MUST go to the transcript.
Focusing the transcript MUST NOT scroll the page or the transcript.
Choosing a thread in the resume panel MUST keep the panel open until the attempt to open that thread ends.
While that attempt runs, the resume panel that started it MUST disable its thread choices.
During that attempt, the resume panel that started it MUST show `Opening thread…` as status text inside its dialog.
A resume panel that opens while an attempt from an earlier resume panel runs MUST enable its thread choices.
That resume panel MUST NOT show `Opening thread…` for the earlier attempt.
An action that a panel starts and that ends after that panel closed MUST NOT close the panel that is open then.
A panel MUST keep its title and footer inside the viewport.
When a panel is taller than the viewport, only its body MUST scroll.
`web/checks/panel-footer.js` checks these rules for the open panel and for each provider choice of the login panel.

The resume panel filters thread titles, previews, and identifiers as the user types.
Conversation search starts after a 300 ms pause only when the filter contains at least one Unicode-whitespace-separated term.
Every term MUST contain at least three Unicode scalars, matching the daemon's conversation search eligibility rule.
Padding and separators MUST NOT count toward a term's minimum length.
The search MUST send the unchanged filter. Derived local matching keys MUST NOT change the field or result identities.
`ab ` and `abc de` MUST stay in local filtering without a search request. ` abc ` MUST search with its spaces preserved.
Each new keystroke cancels the pending search and discards its results.
The client discards search results from an earlier connection.
The `Matches in conversations` section shows each matching title and snippet.
Selecting a match opens its thread and closes the panel.
An empty search shows `No conversation mentions this text.`
A failed search shows `Search failed: <cause>.` and a Try again button.

The Settings panel MUST save only the settings whose values changed, in one `config/batchWrite` call.
The Settings panel MUST send the key paths `approval`, `verbosity`, `theme`, `statusHide`, and `editReview`.
An empty or unknown choice MUST NOT be saved, and the panel MUST show `Choose a <label>.` in its body.
After a save, the client MUST apply the theme at once and MUST reload the settings.
A save that changes `approval` MUST show `saved; open threads keep their approval and sandbox`.

The login panel MUST list each provider from `dalgona/account/providers/list`.
An API key MUST be sent with `account/login/start` only after the user types it.
The field MUST preserve accepted key text exactly. Invalid provider syntax MUST produce an error without trimming, normalizing, or changing the key.
A hybrid provider MUST show the API key field and the OAuth shell command.
A failed API-key login MUST keep the panel open and MUST show the error under the key field.
A browser or device-code login MUST wait for `account/login/completed` with its `loginId`.
A successful login MUST show `Signed in to <provider>`, reload the models, and close the panel.
A failed browser or device-code login MUST show `Login failed` with a Try again action.
The login panel MUST show a Close button in every state, so that a pointer or touch user can leave it.
Closing the login panel or opening another panel MUST cancel a pending browser or device-code login with `account/login/cancel`.
A start response that arrives after the cancel MUST be cancelled with the `loginId` it returns.

The model panel MUST list each visible model under `Models`, with its description.
The model panel MUST list each preset under `Presets`, as the preset, the model, and its default effort.
Choosing a preset MUST fill the model and its default effort.
The model panel MUST save the model and its effort in one `config/batchWrite` call, with the key paths `model` and `model_reasoning_effort`.
The model panel MUST NOT save a model that the list lacks, or an effort that the model does not offer.
Saving a model MUST NOT change the approval or sandbox of the open thread.
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

A scrolling region MUST NOT take a tab stop of its own.
While the title or an action of a request card has focus, ArrowUp, ArrowDown, PageUp, PageDown, Home, and End MUST scroll its details.
Inside the details, those keys MUST keep their native meaning.
Each thread button MUST be a native button, so Tab reaches it and the browser scrolls the thread list to it.

Each visible control MUST be at least 24 by 24 CSS pixels. A button MUST be at least `--control-height` tall, and at least `--control-height-coarse` tall under a coarse pointer. `web/checks/target-size.js` checks these rules.

Form text MUST be at least 16 pixels. `web/checks/input-size.js` checks this rule.

Under `prefers-reduced-motion: reduce`, animations MUST stop, and the elapsed text MUST NOT change without a new item. `web/checks/reduced-motion.js` checks this rule.

Under forced colours, each state tag and the point button MUST keep a visible border. `web/checks/forced-colors.js` checks this rule.

The page MUST have one status region and one alert region. They MUST be the notice regions.
The transcript MUST NOT be a live region.
The compact, fork, tree, rewind, advisor, review, and resume panels MUST show their own error as an alert inside their dialog. They MUST NOT add a notice for it.
The resume panel MUST show its search progress as status text inside its dialog. It MUST NOT add a notice for it.
A failed save in the model or settings panel MUST show a notice.
The composer MUST show its error as an alert beside the message field. The message field MUST name that alert in `aria-describedby`. The composer MUST NOT add a notice for its error.
The `page live regions` tests in `web/src/lib/components/App.dom.test.ts` check these rules.
`web/checks/approval-actions.js` checks the approval card at 320x568, 375x812 and 667x375. It checks that the card holds its actions and that they do not cover the composer, at both ends of the dock's scroll. It checks that the composer shows its label and one field row, and that it is no taller than that while the dock scrolls. `web/checks/dock.js` checks the composer dock at the start, the middle and the end of the document. `web/checks/sidebar-cap.js` checks the thread list cap and its scroll. `web/checks/status-band.js` checks the band drops and keeps items at narrow widths. `web/checks/band-rows.js` checks, in a window wider than 640 px, that no band item drops and every row ends at the band's right edge. `web/checks/forced-colors-selection.js` checks the selected thread border under forced colours. `web/checks/skip-link.js` checks the skip link to the transcript at 1280x800, on a thread whose transcript holds more than its column shows.
