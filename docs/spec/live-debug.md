# Live debugging

Live debugging runs both products against a scripted chat-completions server. The server needs no credentials and no network. This topic defines the server, its script, and the stream framing it sends.

## Scripted server

The `internal/llmstub` package MUST serve `POST /v1/chat/completions` and `GET /v1/models`. Any other path MUST return HTTP 404. A known path with another method MUST return HTTP 405.

Both endpoints MUST require `Authorization: Bearer <key>`. A missing or wrong token MUST return HTTP 401 with the code `llmstub_auth`.

The server MUST pick the first turn in script order whose `when` matches the request. A turn with `once` set MUST match only one request.

A request that matches no turn MUST return HTTP 400 with the code `llmstub_unmatched`. The client MUST treat that 400 as terminal, as [providers.md](providers.md) requires for other errors.

The server MUST write one JSON line for each chat request to its record writer before it replies. The line holds the sequence number, the path, the turn id or `unmatched`, and the request body.

The server MUST write `llmstub: <seq> <turn> <path>` to its log writer for each chat request.

A `status` turn MUST reply with its status code and its body, with `Content-Type: application/json`. A `retry_after` value MUST set the `Retry-After` header.

## Script format

A script MUST hold 1 to 8 models and 1 to 256 turns. Each model id MUST be non-empty and unique.

A turn id MUST match `^[a-z0-9-]{1,40}$`. A turn id MUST be unique in the script.

A turn MUST set exactly one `when` key: `user`, `tool_result`, or `any_text`. The `user` key MUST match when the last message is from the user and contains the value. The `tool_result` key MUST match when the last message is a tool result with that call id. The `any_text` key MUST match when any message contains the value.

A turn with a `status` MUST have no steps. Its `body` MUST be valid JSON. A `status` MUST be 0 or from 400 to 599.

A `retry_after` MUST appear only with status 429 or 503. It MUST have 1 to 5 digits.

A turn with no status MUST have 1 to 512 steps. Each step MUST have exactly one kind: `text`, `reasoning`, `tool_call`, `delay_ms`, `finish`, `error_frame`, or `drop`. The `usage` key MUST appear only with `finish`.

A `finish`, `error_frame`, or `drop` step MUST be the last step. The last step MUST be one of these three kinds.

A `tool_call` MUST name an id, a name, and a JSON object as `arguments`. Its `chunks` MUST be from 1 to 16. A missing `chunks` value means 1.

A `delay_ms` value MUST be from 0 to 120000.

A script error MUST name the file, the turn number, and the step number. Turn and step numbers start at 1.

## SSE framing

Each frame MUST be one line that starts with `data: `, followed by one JSON value and a blank line.

A `text` step MUST send a frame with `delta.content`. A `reasoning` step MUST send a frame with `delta.reasoning_content`.

The first frame of a tool call MUST carry the call id, the type `function`, and the name. Later frames of that call MUST carry only the index and a piece of the arguments.

The arguments MUST split into the chunk count at UTF-8 character boundaries. Each piece MUST hold at least one character.

A `finish` step MUST send a frame with `finish_reason` and `usage`, then `data: [DONE]`. An `error_frame` step MUST send an error frame, then `data: [DONE]`.

A `drop` step MUST end the response without `data: [DONE]`.

A `delay_ms` step MUST end when the client cancels the request.

## Command

The command `internal/llmstub/cmd/llmstub` MUST accept the flags `-listen`, `-script`, `-key`, and `-record`.

The `-listen` flag MUST default to `127.0.0.1:18080`.

The `-script` and `-key` flags MUST be set. The `-key` value MUST NOT be empty.

The `-record` flag MUST name a file that the command appends to. The command MUST create a missing file with mode 0600.

After it listens, the command MUST write `llmstub: serving http://<address>/v1` to standard error. It MUST write nothing to standard output.

A flag error MUST exit with status 2 and print `llmstub: <error>` to standard error.

A script, record, or listen error MUST exit with status 1.

On SIGINT or SIGTERM, the command MUST stop serving. It MUST wait at most 5 seconds for active requests. If the wait completes, the command MUST exit with status 0. If it does not, the command MUST exit with status 1.

The command MUST allow 10 seconds for a client to send its request headers.

## Run recipe

`just live-up` MUST stop with an error when the server named in `/tmp/dg/llmstub.pid` still runs.
`just live-up` MUST remove `/tmp/dg` and build `dal`, `dalgona`, and `llmstub` into `/tmp/dg/bin`.
`just live-up` MUST copy the workspace fixture to `/tmp/dg/ws` and commit it to a git repository.
The workspace fixture MUST hold the executable `fail-output.sh` with file mode 0755, so the copy keeps it executable.
Any other live setup MUST copy `fail-output.sh` to the workspace root with mode 0755.
`just live-up` MUST write one configuration for each product. The `dalgona` configuration MUST set `[edit_review] feedback` to `lost_code`.
`just live-up` MUST start the scripted server on `127.0.0.1:18080` and record its requests to `/tmp/dg/requests.jsonl`.
`just live-up` MUST start the daemon and `dalgona web` with isolated homes.
The daemon and `dalgona web` MUST use `/tmp/dg/ws` as their working directory.
A new web thread MUST therefore read the fixture workspace.
`just live-up` MUST write the web address to `/tmp/dg/web.url` and the environment exports to `/tmp/dg/env`.
`just live-check` MUST run `just live-up`, then one `read` turn through `dal` and one through `dalgona`.
`just live-check` MUST pass only when two requests name the turn `t-read-2` and no request is unmatched.
`just live-check` MUST pass only when the web relay answers 401 without a token and the web page answers 200.
`just live-check` MUST run `just live-down` when it exits.
`just live-check` MUST exit with status 1 without starting anything when a stack is in use.
`just live-check` MUST set its `just live-down` trap before it runs `just live-up`, so a failed start is stopped.

## Stopping

`just live-down` MUST send `SIGTERM` only to a process whose command line starts with `/tmp/dg/bin/`.
`just live-down` MUST wait up to 6 seconds for each process it stops.
`just live-down` MUST end the tmux server on the socket `dglive`.
`just live-down` MUST remove the pid files of the processes it stops.
`just live-down` MUST exit with status 1 when a process still runs or when the daemon socket remains.

## tui tool

`just omp-tui-install` MUST copy the `tui` tool files from `.references/oh-my-pi/.omp/tools` into `.omp/tools`, which git ignores.
`just omp-tui-install` MUST install the tool dependencies with `bun install --frozen-lockfile`.
The omp host MUST restart with `/restart` after the install, because omp loads custom tools when a session starts.
A check MUST show the `tui` tool in the tool list, and `{"op":"list"}` MUST answer `no sessions`.
A terminal session MUST start with `timeout: 1`, because the app serves no debug socket.
When the `tui` tool is absent, a session MUST use tmux on the socket `dglive`, as the table below shows.

| tui operation | tmux equivalent |
|---|---|
| start | `tmux -L dglive new-session -d -s dg -x 100 -y 30 "env -i PATH=$PATH TERM=xterm-256color COLORTERM=truecolor $(sed 's/^export //' /tmp/dg/env) /tmp/dg/bin/dalgona -C /tmp/dg/ws"` |
| send literal text | `tmux -L dglive send-keys -t dg -l 'TEXT'` |
| send Enter, Escape, C-c, C-o or Up | `tmux -L dglive send-keys -t dg Enter`, `Escape`, `C-c`, `C-o` or `Up` |
| send raw bytes | `tmux -L dglive send-keys -t dg -H HEX` |
| bracketed paste | `tmux -L dglive set-buffer 'TEXT'`, then `paste-buffer -p -t dg` |
| screen | `tmux -L dglive capture-pane -p -t dg`, with `-S -200` for scrollback |
| raw bytes | `tmux -L dglive pipe-pane -o -t dg 'cat >> /tmp/dg/raw.bin'`, started before the first input |
| resize | `tmux -L dglive resize-window -t dg -x 40 -y 30` |
| shot | not available. The report marks colour and theme rows `Not proven: no tui tool` and checks them in the browser only. |

## Terminal session

The rows run on `dalgona` first. Rows T01 to T12 then run again on `dal`. A scenario passes only when its exact observation is seen.

|#|Feature|Input|Proof at 100 and 40 columns, and transcript assertion|
|---|---|---|---|
|T01|Welcome line, mark, terminal title|start|Line 1 holds the welcome text and the product name, which is the terminal's `mark` item (Design record, Status band). `raw` holds no `고나` (Design record, Product marks). `raw` tail holds an OSC 0 or OSC 2 title sequence naming the thread. At 40 columns the welcome wraps without clipping.|
|T02|Dock: three rows, `?` key list|`?`|A help overlay lists the keys and has one legend line per shown status item, so each icon has a text meaning (Design record, Icons). `\e` closes it. Idle: the dock is the composer, one empty live row and the status band.|
|T03|Status band items, icons and context gauge (Design record, Status band; Design record, Icons)|after T05|One row at 100 columns: at the left the model `qwen-test` (text), the effort glyph, the approval glyph `?`, the sandbox glyph and the folder item `▸ ws` (U+25B8, then the folder name `ws`). At the right the `context` item and the product name. Approval shows `?` (Unicode) for `ask`, from `approval = "ask"` in the run configuration. Context is 1200 of 32000, about 4 percent: Unicode: the percent alone, `4%`, with no glyph, in a field of 4 cells (`  4%`). Nerd: U+E38E (index `round(0.0375 * 14)` = 1), one space, then the percent in the same 4-cell field. Config key `icons` (string, default `auto`, values `auto`, `unicode` or `nerd`, example `icons = "nerd"`): with `nerd` set, `raw` holds the Nerd glyph bytes for the effort and approval items. With `KITTY_WINDOW_ID=1` added to the tool session args, the band stays Unicode unless the terminal's reply names a Nerd Font. The tool's emulator writes XTGETTCAP replies back to the child (`.references/oh-my-pi/.omp/tools/tui.ts:13-15`), and Bubble Tea ignores invalid replies (`github.com/charmbracelet/ultraviolet@v0.0.0-20261008173134-6b8d4baf91b4/decoder.go:1035-1049`), so the raw tail decides it. The Nerd switch itself is `Not proven: needs another host (kitty with a Nerd Font)`. Config key `status_hide` (array of item ids from Design record, Status band, default empty, example `status_hide = ["effort"]`): after the daemon restarts, the effort item is gone and the rest stay. The key comes from the protocol workstream, Step 8. The daemon loads its config once at start (`cmd/dalgona/daemon.go:48`). With `status_hide = ["model"]` it exits 1, and `/tmp/dg/daemon.stderr` holds `status_hide: model cannot be hidden`. At 40 columns no text is cut mid-word. The check records the items left after whole-item dropping (the terminal workstream, Step 12).|
|T04|Streaming Markdown, reasoning collapse, sanitizing|`show markdown\r` (t-md)|While streaming, the live row shows only the unfinished tail, on one row. Each block is written to scrollback once, when it is final. After commit at `medium`, scrollback holds the heading, the list, the code block and the table, and thinking is one line. The hostile line shows as `after` with no title change: `raw` does not hold `]0;pwned`, and earlier scrollback is not cleared. The Hangul line shows as typed, `달고나 테스트 문장입니다`, with no `?` in any cell.|
|T05|Tool card, read|`read a.txt\r`|A card shows `read a.txt`, then `Done`. `requests.jsonl` line `t-read-2` holds `hello from a.txt`.|
|T06|Working indicator, live tag, elapsed counter|`take your time\r`|The live row shows the word `Working`, the live dot (U+25AA or U+25AB, switched every 500 ms) and the elapsed counter in its 7-cell field. The live tag is the point (Design record, The point and the thread hue) while no decision is open. The terminal workstream, Step 9 (`liveTag`), draws it. It fills with the thread hue (Design record, The point and the thread hue), and its text takes the point ink. With `NO_COLOR` it is reverse video (`\e[7m`). Two `screen` captures 1.5 s apart differ only in the counter and the dot.|
|T07|Visible follow-up queue|during T06: `and then this` then alt+enter (bytes `\e\r`). Enter during a running turn steers, as the Keys table of `docs/spec/tui.md` says|The dock shows the queued follow-up, and the status band's queue item reads `+  1` in Unicode mode (Design record, Icons): the sign and the count fill a field of 4 cells. Enter during T06 sends `turn/steer` and queues nothing. After `Finished.` the queued follow-up is sent: the next record (turn `t-followup`) holds `and then this`.|
|T08|Esc interrupt guard|during a new t-slow: `\e`|The live row shows `Esc again to interrupt` in place of the state word (the terminal workstream, Step 13). A second `\e` within 2 s makes the live row read `Stopped`, and the stub log shows the connection closed before `Finished.`|
|T09|Approval card, bounded panel|`approve a long command\r`|At `rows: 12` and 40 columns the card's title, the first command lines and the approve and decline actions are all on screen. A `+N lines` marker or a scroll cue shows that more lines exist. Decline with `n`, which declines at once (cross-phase decision 44), and the item reads `Declined`. While the card is open the live row reads `Waiting`, the state `awaiting approval` (`internal/tui/model.go:22`, set at `model.go:234`).|
|T10|Verbosity medium (default)|`run the counter\r`, then `y`|Scrollback holds `31` to `40`, not `1`. `\x0f` (ctrl+o) opens review with `1` to `40`. `\e` leaves, and `raw` counts `\e[?1049h` and `\e[?1049l` once each.|
|T11|Verbosity high and low through `/settings`|`/settings\r`, `down`, `right` (medium to high), `enter`, run T10's prompt again. Then `/settings\r`, `down`, `left`, `left` (high to low), `enter`|`high`: scrollback holds `1` to `40`. `low`: one summary line for the tool group.|
|T12|Failed item keeps its last lines|`run a failing command\r`, then `y`|Even at `low`, the item reads `Failed` with `boom` and the exit code visible. `Failed` is bold (an SGR 1 before the word in the raw tail) and has no fill (Design record, The point and the thread hue). The scripted turn runs `./fail-output.sh` directly, so the row shows that command. The fixture prints `1` to `30`, then `boom` on standard error, and exits with status 3. The turn MUST NOT use `sh -c`, because the `nested-shell` rule declines it before the approval card opens.|
|T13|Diff in fileChange|`edit notes\r`, then `y`|The card names `notes.md` with `-old line` and `+new line`.|
|T14|Plan panel|`make a plan\r`|Three steps with the text states `Done`, `Working`, `Pending`. At 40 columns each step wraps.|
|T15|Subagent block|`spawn a helper\r`|A subagent block shows the child working, then done. Records hold `t-sub-child` and `t-sub-2`.|
|T16|Ask tool panel|`ask me\r`|The questions show with `Orange` first, descriptions, a preview, multi-select marks and an `Other` row. Press `1` (Orange, the first option), `space` on `a.txt`, `down`, `space` on `notes.md`, then `s`. The `t-ask-2` record's tool content holds the answers. Then repeat and cancel with `\e`: the tool content is plain text, not an error.|
|T17|Slash palette|`/`|The palette lists commands. Typing `mo` filters to `/model`.|
|T18|`/model` and presets|`/model\r`|The picker lists `qwen-test` and `qwen-small` (from `GET /v1/models`). Pick `qwen-small`. The status band changes, and the next record has `"model":"qwen-small"`. Send `read a.txt\r` on `qwen-small`, then switch back.|
|T19|`/login` API-key panel|`/login\r`, choose `openai`, type `sk-live-test\r`|The panel closes when `account/login/completed` arrives. `/tmp/dg/fakehome` stays empty, and `/tmp/dg/dgh/auth.json` exists with an `openai` entry. No network request leaves the host, so nothing is added to `requests.jsonl`.|
|T20|Every other slash command|`/new`, `/fork`, `/compact`, `/revert`, `/resume` and the rest listed by T17|Each opens a panel and is navigable with arrows, Enter and Esc. `/revert` asks for confirmation. Run mid-turn (during t-slow), `/new`, `/compact`, `/fork`, `/resume`, `/tree` and `/revert` are refused. The live row shows the notice, for example `/compact is unavailable while a turn is working; wait for it to finish or interrupt it first`, and the full text stays in history. On a thread that holds more content than the kept window (`compaction_keep_tokens`, 20000 tokens by default), `/compact` produces a `t-compact` record, and the context gauge shrinks. The live thread holds less. On it `/compact` shows `compact failed: rpc error -32600: compaction: nothing to summarize` and writes no record, which is the expected result there. `TestOverflowPrunesCompactsAndRetriesOnce` in `internal/agent` checks the compaction and the refusal.|
|T21|Multi-line editing|`first line`, `\x0a` (ctrl+j), `second line\r`|The composer shows two lines. The `t-multi` record holds `first line\nsecond line`.|
|T22|History|`\e[A` on an empty composer|The last prompt comes back.|
|T23|`@file` completion|`@no\t`|Completes to `@notes.md`.|
|T24|Paste chip|bracketed paste of 30 lines with first line `PASTE-MARK-30`, then `\r`|The composer shows the chip `[Paste #1 +30 lines]`, and the `t-paste` record holds all 30 lines.|
|T25|Image paste|bracketed paste of `/tmp/dg/ws/pixel.png`, then `describe the image\r`|The composer shows the chip `[Image #1 1x1]` (the terminal workstream, Step 31). The `t-image` record holds the image part that T46 checks.|
|T26|`!` shell mode|`!ls\r`|The output lists `a.txt` and `notes.md`, and no record is added. The stub log holds no `unmatched` line.|
|T27|A prompt that starts with `/`|`//etc/hosts is a path\r`, where a prompt that starts with `//` is sent with one slash removed (the slash rule of the terminal workstream)|The record's user text starts with `/etc/hosts`.|
|T28|Mouse|in review: `\e[<64;10;5M`|The review scrolls. A click selects nothing (cross-phase decision 47): `\e[<0;10;5M\e[<0;10;5m` leaves the review and the dock unchanged.|
|T29|Rewind gesture|`\e` twice on an empty idle composer (the terminal workstream, Step 33), then `\r` on the newest prompt and `\r` on the confirm|A rewind picker lists earlier prompts. Choosing one restores it into the composer.|
|T30|Notifications|in a session launched with `TERM=xterm-kitty` in its args: `\e[O`, `take your time\r`, wait 31 s|`raw` tail holds `\e]99;` (OSC 99, cross-phase decision 49). The `TERM` rule is in the terminal workstream, The schemas of this workstream.|
|T31|Retry and terminal provider errors|`rate limit short\r`, `rate limit long\r`, `rate limit over\r`, `quota please\r`, `break the stream\r`, `drop the stream\r`|(1) Two records about 2 s apart (`t-429`, then `t-429-ok`), then `Recovered.`. (2) With the client attached, `rate limit long` (Retry-After 120) shows `rate limited · retrying in 2m 00s · esc stops`, and the count drops each second. `\e` stops the wait at once (the terminal workstream, Step 13), with one record. (3) `rate limit over` (Retry-After 301) stops at once with `rate limited for 301 s, over the 300 s wait limit`, and one record. (4) `quota please` is a terminal quota error, with one record. (5) and (6) the row reads `Failed`, and no retry record. Headless, `dal -p "rate limit long" --json` stops at once; its JSON status line holds `rate limited for 120 s, over the 60 s wait limit`, and the stub log holds one record (`-p` opens an RPC connection through `env.connect` (`internal/app/cli.go:457-460`, `:592-593`), and the server calls `s.attach` for a thread at `internal/rpc/serve_thread.go:312` and `:395`. The attended-wait rule (the core-fixes workstream, Step 1a) is not in the code at this commit, because `internal/` has no `RetryWatch` or `retry/wait`, so this row stays unproven until that step lands).|
|T32|Effort and `ultrathink`|`ultrathink about it\r`|The effort icon in the status band shows the raised level for this turn only (Design record, Icons glyph for that level). The `t-ultra` record carries the effort field the chatcompat encoder sends, absent or lower on the next turn (`reasoning_effort`, sent only when the effort is set and is not `default`: `internal/model/chatcompat/chatcompat.go:142-143`. The expected value is the level the status band shows for this turn, and the next turn's record has no `reasoning_effort` key).|
|T33|Six theme settings, auto detection, `NO_COLOR`, `DAL_NO_MOTION`|`/settings`, `down`, `down`, then `right` steps the theme through `auto`, `auto-soft`, `day`, `soft-day`, `night` and `soft-night` (config key `theme`, string, default `auto`, example `theme = "soft-night"`). Then sessions `dg-nocolor` and `dg-nomotion`|`shot` at each step while the panel is open (the preview applies at once), after t-md. `auto` resolves to `night` and `auto-soft` to `soft-night`, because the emulator's background is dark, `0x101418` (`tui.ts:180`). In every theme, no cell outside the point carries a background sequence. The `raw` bytes for those cells hold no `\e[48;`. The point is the only filled cell (the terminal workstream, Step 9). `dg-nocolor`: no colour sequence reaches `raw`. The `colourSGR` check of the terminal workstream, Step 9, finds no SGR parameter in 30 to 38, 40 to 48, 90 to 97 or 100 to 107. The point is reverse video (`\e[7m`), and state words remain. `dg-nomotion` during t-slow: `raw` with `clear: true`, wait 2 s, `raw` again: the byte count is 0.|
|T34|Exit line, then the `dal` run|`\x03\x03`|The exit line reads `Thread <title> saved · resume with <product> --resume <id>` (the terminal workstream, Step 37), with `dalgona` or `dal` as the product. Then repeat T01 to T12 on session `dal` and check the `t-*` records again.|
|T35|Narrow mid-turn, empty composer|`take your time\r`, then `resize` to `cols: 40` during the turn, then `resize` back to 100|At 100 columns the live row shows `Working` and the elapsed time. At 40 columns the status band keeps the approval glyph `?` and the model name cut with `...`. No line is wider than 39 cells. No row of the old dock stays above the new dock. `screen` with `peek: 300` shows the `t-slow` text once.|
|T36|Narrow with one long line in the composer|send 120 `x` characters without Enter, then `resize` to `cols: 40`|The composer wraps to rows of at most 39 cells, and the cursor stays on the row that holds the insertion point. No dock row is wider than 39 cells. The terminal may rewrap rows of the earlier, wider dock above the cursor. Those rows are terminal history, not dock rows, so they are not judged. `screen` with `peek: 300` shows no history row twice.|
|T37|Narrow with three composer lines|`first`, `\x0a`, `second`, `\x0a`, `third`, then `resize` to `cols: 40`|The composer shows three lines, and the cursor stays on its line. The dock below the cursor is complete, and each dock row is at most 39 cells. Rows above the cursor line that the terminal rewrapped are terminal history. The check reads the rows of the dock and the typed lines only. `screen` with `peek: 300` shows no history row twice.|
|T38|Open and close each panel|`/settings\r` then `\e`. `/model\r` then `\e`. `/login\r` then `\e`. `/resume\r` then `\e`. `approve a long command\r`, then `n`. `ask me\r`, then `\e`. `\x0f`, then `\e`|Each panel opens between the composer and the live row (cross-phase decision 53). The dock is taller only while a panel is open. After each close, `screen` shows the earlier dock height, and the rows above the dock match the capture taken before the panel opened.|
|T39|Stream a long answer|`show markdown\r` (t-md), then `screen` with `peek: 300` every 0.5 s until `Done`|Each capture holds the same rows above the dock as the earlier captures, from the first row that was written. Only the live tail row and the dock change during the stream. The status band keeps its row number in every capture (I2: the dock height does not change while the window is the same).|
|T40|Suspend and resume|`take your time\r`, then `\x1a` (ctrl+z) during the turn. Resume from a second shell with `kill -CONT $(pgrep -f /tmp/dg/bin/dalgona)`, because the tool has no job control|After `\x1a` the screen shows one blank row. After the resume the dock is drawn once at the cursor's line, with the `Working` tag and the elapsed time. `peek: 300` holds one copy of each history row.|
|T41|Quit at 40 columns, empty composer|`resize` to `cols: 40`, then `\x03\x03`|The exit row prints once, and the dock is gone. The shell prompt sits below the exit row. Nothing of the dock is printed to the history: the program commits only the committed rows and the exit row. Terminal rewrap of the earlier dock rows is not judged. No line follows the exit row except the shell prompt. A daemon log line that reaches the terminal after the exit row is a failure.|
|T42|Resize while streaming, 100x24 to 40x8 and back (I6, I7)|start with `rows: 24`, send `take your time\r`, and during the turn `resize` to `cols: 40, rows: 8`, then `resize` to `cols: 100, rows: 24`|At 8 rows the status band is the last row, and every line is at most 39 cells. No row of the old dock stays above the new dock. After the resize back, `screen` with `peek: 300` shows each history row once, and the dock is complete.|
|T43|300-line tool output (I5, I2)|`count to 300\r`, then `y`|At medium the scrollback holds `291` to `300` and not `1`. `\x0f` opens the review with `1` to `300`. `screen` with `peek: 300` shows the `300` row once. The dock height is the same before and after the output (I2).|
|T44|No erase or home sequence (I4)|`raw` with `clear: true` before `show markdown\r`, read after `Done` (T39)|The captured bytes hold none of `\e[2J`, `\e[3J`, `\e[H` or `\ec`, because the alternate screen is off in that session.|
|T45|One tick writes only fixed fields (I8)|`take your time\r`, then `raw` with `clear: true`, wait 1 s, then `raw` again|Each write in the second capture is a cursor move to the live row, then the live dot, the state word or the counter. No other row is written. With `DAL_NO_MOTION=1` the dot writes nothing, so the second capture holds one cursor move to the live row and the changed cells of the elapsed field, in columns 11 to 17. The byte count is greater than 0, and no write lands in columns 1 to 10.|
|T46|Image part on the wire (the image-input workstream)|the T25 paste, then `describe the image\r`|The `t-image` record's last user message holds one image part in the wire form that the image-input workstream defines. The part's data is the base64 bytes of `pixel.png`.|
|T47|No dock row in the tmux history (print rules 4 and 5)|in tmux 3.6 with `scroll-on-clear on`, start the client as the command of a new window (`tmux new-window '<binary>'`), open and close the model panel, send `hello\r`, wait for the turn to end, then run `tmux capture-pane -p -S -`|The captured history holds the welcome row, the user row and the reply rows once each, and no line of the dock (no composer line, no status line, no panel line) above the last screen.|
|T48|Print path in tmux: no dock row in the history|start the client as the command of a new window (`tmux new-window '<binary>'`), then `alt+q`, then `esc`, then `take your time\r`, wait for `Done`, then `tmux capture-pane -p -S -`|No dock row appears in the history. Each printed row appears once, in order.|

## Browser session

Open `$(cat /tmp/dg/web.url)` with omp's `browser` tool. Set the viewport per pass with the browser tool's emulate state, `viewport` (`.references/oh-my-pi/packages/coding-agent/src/tools/browser/emulation.ts:46-48`). Reduced motion uses the tool's `reducedMotion` emulation and the colour scheme its `colorScheme` emulation (`.references/oh-my-pi/packages/coding-agent/src/tools/browser/emulation.ts:186-195`). Forced colours are not run in the live tab. The browser tool's emulate state has no forced-colours key (`emulation.ts:186-195` reads `colorScheme` and `reducedMotion` only), and the page handle is private. W24 runs the repo gate `web/checks/forced-colors.js` (the web workstream, Step 38) instead.

The feature checks run once at 1280 px in `day`. The layout matrix runs every width (320, 768, 1280) in each of the four themes, chosen through the web theme setting. Each matrix cell takes a full-page screenshot and runs these probes:

- `document.documentElement.scrollWidth <= innerWidth`
- every button and field has `getBoundingClientRect().height >= 36` (Design record, Components, `min-height: 36px`)
- exactly one element in the view carries the solid point fill (Design record, The point and the thread hue). The product mark does not count. The selector is `.point, button.primary`, which the web workstream, Steps 8 and 10 use for the point fill.
- no element with `getBoundingClientRect().right > innerWidth + 0.5` inside `.shell`
- `document.fonts.check('16px "Hanken Grotesk"') && document.fonts.check('16px "Bricolage Grotesque"')`

|#|Feature|Action|Proof|
|---|---|---|---|
|W01|Connect, token, empty states|load the URL|The idle state word is `Ready`. No row shows `Offline`. The transcript says `Pick a thread or start a new one`. The page icon is the candy mark, `dalgona-small.svg` (Design record, Product marks), served at `/dalgona-small.svg`: `internal/webui/webui.go:155` serves each file under `dist` at its path, `web/static/` is not in the checkout at commit `3c4b6f94`. The web workstream, Step 9 creates it and copies the icon, so the icon check fails until that step lands. `curl` without a token gets 401 (in `live-check`).|
|W02|Live regions|load|`[aria-live]` nodes exist at load with empty text. No `role=log` sits on the streaming delta container.|
|W03|Streaming Markdown, thinking shown, sanitizing|send `show markdown`|Heading, list, code and table render. Thinking collapses. The hostile line renders as text, and `document.title` is unchanged.|
|W04|Tool card, read|`read a.txt`|The card reads `Done`. The `t-read-2` record exists.|
|W05|Status band (Design record, Status band; Design record, Icons; Design record, Product marks)|after W04|The margin column is right-aligned with three rows. The first holds `model` as text. The second holds `effort approval sandbox folder queue`. The third holds `context mark`. Every icon has an `aria-label` that states its value in words, and the folder icon also has `title` and `aria-label` with the folder name. The icons draw Nerd code points from the subset font whatever `icons` holds: effort, approval and sandbox from the Design record, Icons table, and the context gauge as U+E38D to U+E39B at index `round(f * 14)`. The percent sits beside the gauge, and its element computes `font-variant-numeric` containing `tabular-nums` (Design record, Type for the web). The mark is `dalgona-small.svg`, and `document.body.innerText` does not contain `고나` (Design record, Product marks). At 320 px the band drops whole items by rank (the web workstream, Step 20, product decision 15). Record the items left and check that no text is cut mid-word.|
|W06|Working tag, motion|`take your time`|The `Working` tag, the view's point (Design record, The point and the thread hue) in the thread hue (Design record, The point and the thread hue), has the breathing dot and a ticking counter (`document.getAnimations().length > 0`).|
|W07|Follow-up queue|during W06, send `and then this`|The queue item shows the Nerd glyph U+F051F (`md-timer_sand`) then the count `1` (Design record, Status band; Design record, Icons). The follow-up is sent after the turn.|
|W08|Interrupt|during a new t-slow, Interrupt|The state tag reads `Stopped`. Focus moves to the composer, not `body`.|
|W09|Approval dialog|`approve a long command`|At 320 px and at 768 px the actions stay visible while the 50-line command scrolls (the actions are sticky). The approval-queue note is inside `.shell`. Decline uses the quiet style. The approve action is the view's point (Design record, The point and the thread hue), the only solid accent fill in the dialog, and its buttons are at least 36 px tall (Design record, Components).|
|W10|File-change approval diff|`edit notes`|The dialog shows the `notes.md` diff with no 16rem cap cutting `+new line`, and lists the file. In `night`, the added and removed text reach 4.5:1 against their row ground, computed from the page colours (Design record, Components).|
|W11|Plan checklist|`make a plan`|The text states appear, and `aria-current="step"` is on the in-progress step.|
|W12|Subagent, child approvals|`spawn a helper`|The child block appears in the open thread.|
|W13|Ask tool|`ask me`|Option list with `Orange` first, descriptions, preview, multi-select and `Other`. No `<form>`, `<input type=password>` or `<img>` inside the panel. The answer reaches `t-ask-2`.|
|W14|Settings, model, login panels|open each|Each validates on submit, has keyboard navigation (Tab, arrows, Esc) and no dead end. The model list shows both models. Login with `openai` and `sk-live-test` closes the panel when `account/login/completed` arrives.|
|W15|Retry and errors|the six error prompts from T31|The same texts as T31, with the countdown for `rate limit long` (Esc stops it). The short 429 shows the two records of T31 (1).|
|W16|Reconnect|`kill -TERM $(cat /tmp/dg/daemon.pid)`, wait, start the daemon with the `live-up` daemon line|The error text names Reconnect and a visible Reconnect button exists. Clicking it resumes the open thread, and queued follow-ups are kept.|
|W17|Selected thread not colour-only|open two threads|The selected item has `aria-current` and a non-colour cue that stays visible under forced colours. Each thread row has an 8 px dot in its Design record, The point and the thread hue hue, index `fnv1a32(threadID) % 4`, and the dot's colour is one of the four hues of the active theme. The selected row has a 2 px bar in the `text` role on its leading edge, as well as the `tone` fill (Design record, Components).|
|W18|Jump to latest|`long output`, scroll up 400 px|A jump-to-latest control appears. Clicking it brings the end into view.|
|W19|Sidebar cap at narrow widths|768 and 320 px|The thread list's own scroll box is capped at 18rem, not its wrapper.|
|W20|Context percent|after W04|The `context` item's percent changes after `thread/tokenUsage/updated`. No token count is shown (cross-phase decision 33).|
|W21|Edit review (opt-in)|enable it in settings, `edit notes` with `t-judge` and `t-judge-turn`|The file change shows `Flagged: removed code`, the terminal shows a `review` row with that text, and the working model's second request body contains `<edit-review call=`. With the setting off, nothing appears. The judge is `local/qwen-test` through `[approval_decider]` (the section "Schemas of the live-debug workstream"). The judge call is the edit-review workstream, Step 4.|
|W21b|Turn review (opt-in)|Enable review, then send `edit notes` with `t-judge`, `t-edit-review-continue`, and `t-judge-turn`.|The feedback request matches `t-edit-review-continue` and receives a normal reply. Exactly one `t-judge-turn` request follows. One plain line after the turn's last message reads `No problems found` in dim text. With the setting off, no line appears. The judge is `local/qwen-test` through `[approval_decider]`. The judge call is the edit-review workstream, Step 4. Judge routes precede the feedback route. Unrelated feedback has no fallback.|
|W22|Ratchet (dalgona)|Send `start a ratchet` through `t-ratchet`, `t-ratchet-2`, and `t-ratchet-approved`.|Init uses `a.txt` as cases, `src/main.go` as harness, and `notes.md` as change paths. These paths do not overlap. The next direct call approves inputs. The ratchet approval question opens the ask panel. The card shows `Approve`, `Revise`, and `Abort`. Select `Approve`. The final scripted reply confirms receipt of the approval result.|
|W23|Sandbox unavailable notice|run only when the report header says Landlock is unavailable (contingency C1)|Run only on a host without Landlock. The choice is asked once per thread (cross-phase decision 43): a second command in the same thread does not ask again, and the answer holds for that thread.|
|W24|Forced colours|the repo gate `web/checks/forced-colors.js` at 1280 and 320 px (the web workstream, Step 38)|After each Tab press, the focused element's computed outline is `solid`, 2 px wide, with a 2 px `outline-offset`, in the normal themes. Under forced colours `outlineStyle` is not `none`. State tags keep their words. Borders mark the cards. Its report row reads `Proven by test only (web/checks/forced-colors.js)`.|
|W25|Reduced motion|`reducedMotion: true` during t-slow|`document.getAnimations().length === 0`, and the counter holds still across 2 s.|
|W26|Themes|the 3x4 matrix|12 screenshots. Tags, buttons and status band follow `DESIGN.md` tokens. No gradient appears: no computed `background-image` contains `gradient`. The matrix uses the four concrete themes. `auto` gives `night` under dark and `day` under light. `auto-soft` gives `soft-night` under dark and `soft-day` under light. The page background (the `canvas` role) equals `#111113`, `#212225`, `#FFFCF0` and `#E6E4D9` for night, soft-night, day and soft-day (Design record, Colour).|
|W27|Hangul and fonts|t-md|The Hangul line renders in Pretendard: `document.fonts.check('16px Pretendard', '달')`.|
|W28|Focus after removed controls|approve or decline, then close panels|`document.activeElement !== document.body` after each.|
|W29|Seq guard and schema validation|Vitest only|Covered by the Vitest tests of Web step 2, because the live server cannot reorder messages.|
|W30|Codex methods|from the browser console over the open socket: `fuzzyFileSearch`, `mcpServerStatus/list`, `review/start`, `command/exec`, `config/batchWrite`, `fs/*`|Each answers a result, not "method not found".|

## Defect loop

1. Freeze the evidence: the `screen` or screenshot, `raw` tail, the matching `requests.jsonl` lines, `/tmp/dg/daemon.stderr`, `/tmp/dg/llmstub.log`, and the check id.
2. Name the contract the observation breaks. It is one of: a sentence in `docs/spec/<topic>.md`, the Codex schema in `internal/rpc/testdata/codex-schema`, or a settled product or cross-phase decision. If no contract covers it, write the spec sentence first in the same commit.
3. Pick the lowest layer that shows the defect:
   - Provider framing: a test in `internal/model/chatcompat` built on `internal/llmstub`.
   - Protocol data: `internal/rpc` serve tests.
   - Terminal render or keys: `internal/tui` (the whole-program pattern in `internal/tui/run_test.go`, with `Width` and `Height` set to 40 or 100).
   - Web: a Vitest file next to the component under `web/src`.
   - Daemon or recipe wiring: `cmd/dalgona` tests.
4. Write that test so its expected value comes from the contract named in step 2 of this loop. Run it on HEAD and record the failure line: `go test -run <Name> ./<pkg>` or `pnpm -C web exec vitest run <file>`.
5. Fix, rerun the test, rerun the live check id, then `just verify`. One commit holds the test, the fix and the spec line.
6. Add any script turn the defect needed to `live.json` in the same commit. `TestLiveScriptParses` keeps the script valid.

## Not provable here

What cannot be proven here:

- Real provider wire formats, OAuth and device-code login against a real provider: needs credentials.
- `dalgona service install` starting at login: needs reboot.
- `dalgona codex install|status` and the app-server under the real app: needs the Codex desktop app.
- `--remote`, `cloud up|down|pull` and remote runners: needs a cloud service.
- Windows pinned writes, ConPTY and the macOS peer check: need another host.
- The sandbox-unavailable notice (W23) on a host where Landlock works: needs another host.
- Nerd glyph switch from kitty's font reply (Design record, Icons): needs another host (kitty with a Nerd Font).
- Clipboard image paste through the OS clipboard: needs a desktop session.
- How real terminals display notifications.
- Whether a real decision model judges well.

The report marks each with the matching `Not proven` status.

## Report

`/tmp/dg/report.md` holds one row per check id with the columns check, status, evidence and note. The status is one of:

- `Proven live`
- `Proven by test only (<package>.<Test>)`
- `Failed, defect <test name>`
- `Not proven: needs credentials`
- `Not proven: needs reboot`
- `Not proven: needs Codex desktop app`
- `Not proven: needs cloud service`
- `Not proven: needs another host (<what>)`
- `Not proven: no tui tool`

A row is never marked proven without evidence. The report starts with the HEAD commit, the host kernel, and whether Landlock was available.
