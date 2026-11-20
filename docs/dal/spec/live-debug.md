# Live debugging

This runbook defines the scripted chat-completions server and the manual terminal and browser acceptance matrix for both products. The server uses a local test key and needs no provider credentials or external network. The procedures below are validation plans, not records of executed checks. `just live-check-dal` and `just live-check-dalgona` cover only the read turn and HTTP checks in their run recipe. They do not run the manual matrix.

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

A turn MUST set exactly one `when` key: `user`, `user_exact`, `tool_result`, or `any_text`. The `user` key MUST match when the last message is from the user and contains the value. The `user_exact` key MUST match only when the last message is from the user and its full text equals the value. The `tool_result` key MUST match when the last message is a tool result with that call id. The `any_text` key MUST match when any message contains the value.

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

Each `live-up` recipe MUST create its workspace as a private directory. On startup it MAY reset a directory owned by the current user when that product's previous start left its marker. It MUST refuse a symbolic link, an unmarked path or a workspace claimed by another start. The operator can stop an older run and move its unmarked workspace aside before retrying. A failed start MUST stop any services it started. A completed start MUST leave the workspace marked for reuse.

`just live-up-dal` MUST stop with an error when the server named in `/tmp/dg-dal/llmstub.pid` still runs. It MUST build `dal` and `llmstub` into `/tmp/dg-dal/bin`. It MUST copy the dal workspace fixture to `/tmp/dg-dal/ws` and commit it to a git repository. The fixture commit MUST use the configured global git identity. The workspace fixture MUST hold the executable `fail-output.sh` with file mode 0755, so the copy keeps it executable. Any other live setup MUST copy `fail-output.sh` to the workspace root with mode 0755. `just live-up-dal` MUST write the dal configuration. It MUST set `[providers.openai] base_url` to `http://127.0.0.1:18080/v1`, so the `/login` key check reaches the scripted server. The scripted server MUST use the key `sk-live-test`. `LOCAL_LLM_KEY` MUST hold that same value. `just live-up-dal` MUST start the scripted server on `127.0.0.1:18080` and record its requests to `/tmp/dg-dal/requests.jsonl`. It MUST NOT start a daemon or a web server. It MUST write the environment exports to `/tmp/dg-dal/env`.
`just live-up-dalgona` MUST stop with an error when the server named in `/tmp/dg-dalgona/llmstub.pid` still runs. It MUST build `dalgona` and `llmstub` into `/tmp/dg-dalgona/bin`. It MUST copy the dalgona workspace fixture to `/tmp/dg-dalgona/ws` and commit it to a git repository. `just live-up-dalgona` MUST write the dalgona configuration. The `dalgona` configuration MUST set `[edit_review] feedback` to `lost_code`. It MUST set `[providers.openai] base_url` to `http://127.0.0.1:18081/v1`, so the `/login` key check reaches the scripted server. `just live-up-dalgona` MUST start the scripted server on `127.0.0.1:18081` and record its requests to `/tmp/dg-dalgona/requests.jsonl`. It MUST start the daemon and `dalgona web` with isolated homes. The daemon and `dalgona web` MUST use `/tmp/dg-dalgona/ws` as their working directory. A new web thread MUST therefore read the fixture workspace. `just live-up-dalgona` MUST write the web address to `/tmp/dg-dalgona/web.url` and the environment exports to `/tmp/dg-dalgona/env`.
`just live-check-dal` MUST run `just live-up-dal`, then one `read` turn through `dal`. It MUST pass only when one request names the turn `t-read-2`, at least one tool result holds `hello from a.txt`, and no request is unmatched. `just live-check-dal` MUST run `just live-down-dal` when it exits. It MUST exit with status 1 without starting anything when a stack is in use. It MUST set its `just live-down-dal` trap after `just live-up-dal` succeeds, so a refused start leaves the existing workspace alone.
`just live-check-dalgona` MUST run `just live-up-dalgona`, then one `read` turn through `dalgona`. It MUST pass only when one request names the turn `t-read-2`, at least one tool result holds `hello from a.txt`, and no request is unmatched. It MUST pass only when the web relay answers 401 without a token and the web page answers 200. `just live-check-dalgona` MUST run `just live-down-dalgona` when it exits. It MUST exit with status 1 without starting anything when a stack is in use. It MUST set its `just live-down-dalgona` trap after `just live-up-dalgona` succeeds, so a refused start leaves the existing workspace alone.

## Stopping

An exited process that still has a PID while it awaits reaping MUST count as stopped.

`just live-down-dal` MUST send `SIGTERM` only to a process whose command line starts with `/tmp/dg-dal/bin/`. It MUST wait up to 6 seconds for the process it stops. It MUST remove the pid file of the process it stops. It MUST exit with status 1 when the process still runs. `just live-down-dalgona` MUST send `SIGTERM` only to a process whose command line starts with `/tmp/dg-dalgona/bin/`. It MUST wait up to 6 seconds for each process it stops. It MUST end the tmux server on the socket `dglive`. It MUST remove the pid files of the processes it stops. It MUST exit with status 1 when a process still runs or when the daemon socket remains.

## tui tool

The `.references/` directory is ignored by git (`.gitignore:191`). The oh-my-pi checkout and tool sources below are external prerequisites, not artifacts supplied by this repository. Check their interfaces in the installed version before using the tool-specific instructions. If they are unavailable, use the tmux fallback for terminal probes and record browser probes that cannot run as not proven.

`just omp-tui-install` MUST copy the `tui` tool files from `.references/oh-my-pi/.omp/tools` into `.omp/tools`, which git ignores.
`just omp-tui-install` MUST install the tool dependencies with `bun install --frozen-lockfile`.
The omp host MUST restart with `/restart` after the install, because omp loads custom tools when a session starts.
A check MUST show the `tui` tool in the tool list, and `{"op":"list"}` MUST answer `no sessions`.
A terminal session MUST start with `timeout: 1`, because the app serves no debug socket.
When the `tui` tool is absent, a session MUST use tmux on the socket `dglive`, as the table below shows.

| tui operation | tmux equivalent |
|---|---|
| start | `tmux -L dglive new-session -d -s dg -x 100 -y 30 "env -i PATH=$PATH TERM=xterm-256color COLORTERM=truecolor $(sed 's/^export //' /tmp/dg-dalgona/env) /tmp/dg-dalgona/bin/dalgona -C /tmp/dg-dalgona/ws"` |
| send literal text | `tmux -L dglive send-keys -t dg -l 'TEXT'` |
| send Enter, Escape, C-c, C-o or Up | `tmux -L dglive send-keys -t dg Enter`, `Escape`, `C-c`, `C-o` or `Up` |
| send raw bytes | `tmux -L dglive send-keys -t dg -H HEX` |
| bracketed paste | `tmux -L dglive set-buffer 'TEXT'`, then `paste-buffer -p -t dg` |
| screen | `tmux -L dglive capture-pane -p -t dg`, with `-S -200` for scrollback |
| raw bytes | `tmux -L dglive pipe-pane -o -t dg 'cat >> /tmp/dg-dalgona/raw.bin'`, started before the first input |
| resize | `tmux -L dglive resize-window -t dg -x 40 -y 30` |
| shot | not available. The report marks colour and theme rows `Not proven: no tui tool` and checks them in the browser only. |

## Dal session

Dal runs these rows over the dal fixture with the dal binary only. Dal MUST NOT start a daemon, a web server, a remote runner, or a browser.

|#|Feature|Input|Proof at 100 and 40 columns, and transcript assertion|
|---|---|---|---|
|T01|Welcome line, mark, terminal title|start|Line 1 holds the welcome text and the product name, which is the terminal's `mark` item (`DESIGN.md`, Status band). Product chrome holds no mascot label `고나` (`DESIGN.md`, Product marks). This restriction does not apply to user or model transcript text, including T04's Korean fixture text. `raw` tail holds an OSC 0 or OSC 2 title sequence naming the thread. At 40 columns the welcome wraps without clipping.|
|T02|Dock: three rows, `?` key list|`?`|A help overlay lists the keys and has one legend line per shown status item, so each icon has a text meaning (`DESIGN.md`, Icons). `\e` closes it. Idle: the dock is the composer, one empty live row and the status band.|
|T03|Status band items, icons and context gauge (`DESIGN.md`, Status band, Icons)|after T05|One row at 100 columns. At the left sit the model `qwen-test` (text), the effort glyph, the approval glyph `?` and the sandbox glyph. The effort glyph appears when the model supports at least two levels. Then comes the folder item `▸ ws` (U+25B8, then the folder name `ws`). At the right the `context` item and the product name. Approval shows `?` (Unicode) for `ask`, from `approval = "ask"` in the run configuration. Context is 1200 of 32000, about 4 percent: Unicode: the percent alone, `4%`, with no glyph, in a field of 4 cells (`  4%`). Nerd: U+E38E (index `round(0.0375 * 14)` = 1), one space, then the percent in the same 4-cell field. Config key `icons` (string, default `auto`, values `auto`, `unicode` or `nerd`, example `icons = "nerd"`). With `nerd` set, `raw` holds the Nerd glyph bytes for approval and for effort when visible. The scripted `qwen-test` model advertises no effort levels, so its effort item is absent. With `KITTY_WINDOW_ID=1` added to the tool session args, the band stays Unicode unless the terminal's reply names a Nerd Font (`DESIGN.md`, Icons). Record the reply and the raw tail. Do not infer font support from the environment variable alone. The Nerd switch itself needs another host (kitty with a Nerd Font). Config key `status_hide` (array of item ids from `DESIGN.md`, Status band, default empty, example `status_hide = ["effort"]`). After the daemon restarts, the effort item is gone and the rest stay. With `status_hide = ["model"]` it exits 1, and `/tmp/dg-dalgona/daemon.stderr` holds `status_hide: model cannot be hidden`. At 40 columns no text is cut mid-word. The check records the items left after whole-item dropping (`DESIGN.md`, Status band).|
|T04|Streaming Markdown, reasoning collapse, sanitizing|`show markdown\r` (t-md)|While streaming, the live row shows only the unfinished tail, on one row. Each block is written to scrollback once, when it is final. After commit at `medium`, scrollback holds the heading, the list, the code block and the table, and thinking is one line. The hostile line shows as `after` with no title change: `raw` does not hold `]0;pwned`, and earlier scrollback is not cleared. The Hangul line shows as typed, `달고나 테스트 문장입니다`, with no `?` in any cell.|
|T05|Tool card, read|`read a.txt\r`|A card shows `read a.txt`, then `Done`. `requests.jsonl` line `t-read-2` holds `hello from a.txt`.|
|T06|Working indicator, live tag, elapsed counter|`take your time\r`|The live row shows the word `Working`, the live dot (U+25AA or U+25AB, switched every 500 ms) and the elapsed counter in its 7-cell field. The live tag is the point (`DESIGN.md`, The point and the thread hue) while no decision is open. It fills with the thread hue (`DESIGN.md`, The point and the thread hue), and its text takes the point ink. In the session `dg-nocolor` of T33 it is reverse video (`\e[7m`). Two `screen` captures 1.5 s apart differ only in the counter and the dot.|
|T07|Visible follow-up queue|during T06: `and then this` then alt+enter (bytes `\e\r`). Enter during a running turn steers, as the Keys table of `docs/dal/spec/tui.md` says|The dock shows the queued follow-up, and the status band's queue item reads `+  1` in Unicode mode (`DESIGN.md`, Icons). The sign and the count fill a field of 4 cells. Enter during T06 sends `turn/steer` and queues nothing. After `Finished.` the queued follow-up is sent: the next record (turn `t-followup`) holds `and then this`.|
|T08|Esc interrupt guard|during a new t-slow: `\e`|The live row shows `Esc again to interrupt` in place of the state word ([tui.md](tui.md#keys-and-commands)). A second `\e` within 2 s makes the live row read `Stopped`, and the stub log shows the connection closed before `Finished.`|
|T09|Approval card, bounded panel|`approve a long command\r`|At `rows: 12` and 40 columns the card's title, the first command lines and the approve and decline actions are all on screen. A `+N lines` marker or a scroll cue shows that more lines exist. Decline with `n`, which declines at once ([tui.md](tui.md#approvals)), and the item reads `Declined`. While the card is open the live row reads `Waiting`, the state `awaiting approval` (`internal/dal/tui/state.go:21`, set at `internal/dal/tui/approval.go:46`).|
|T10|Verbosity medium (default)|`run the counter\r`, then `y`|Scrollback holds `31` to `40`, not `1`. `\x0f` (ctrl+o) opens review with `1` to `40`. `\e` leaves, and `raw` counts `\e[?1049h` and `\e[?1049l` once each.|
|T11|Verbosity high and low through `/settings`|`/settings\r`, `down`, `right` (medium to high), `enter`, run T10's prompt again. Then `/settings\r`, `down`, `left`, `left` (high to low), `enter`|`high`: scrollback holds `1` to `40`. `low`: one summary line for the tool group.|
|T12|Failed item keeps its last lines|with verbosity `low` still set from T11: `run a failing command\r`, then `y`|Even at `low`, the item reads `Failed` with `boom` and the exit code visible. `Failed` is bold (an SGR 1 before the word in the raw tail) and has no fill (`DESIGN.md`, The point and the thread hue). The scripted turn runs `./fail-output.sh` directly, so the row shows that command. The fixture prints `1` to `30`, then `boom` on standard error, and exits with status 3. The turn MUST NOT use `sh -c`, because the `nested-shell` rule declines it before the approval card opens.|
|T13|Diff in fileChange|`edit notes\r`, then `y`|The card names `notes.md` with `-old line` and `+new line`.|
|T14|Plan panel|`make a plan\r`|Three steps with the text states `Done`, `Working`, `Pending`. At 40 columns each step wraps.|
|T15|Subagent block|`spawn a helper\r`|A subagent block shows the child working, then done. Records hold `t-sub-child` and `t-sub-2`.|
|T16|Ask tool panel|`ask me\r`|The questions show with `Orange` first, descriptions, a preview, multi-select marks and an `Other` row. Press `1` (Orange, the first option), `space` on `a.txt`, `down`, `space` on `notes.md`, then `s`. The `t-ask-2` record's tool content holds the answers. Then repeat and cancel with `\e`: the tool content is plain text, not an error.|
|T17|Slash palette|`/`|The palette lists commands. Typing `mo` filters to `/model`.|
|T18|`/model` and presets|`/model\r`|The picker lists `qwen-test` and `qwen-small` (from `GET /v1/models`). Pick `qwen-small`. The status band changes, and the next record has `"model":"qwen-small"`. Send `read a.txt\r` on `qwen-small`, then switch back.|
|T19|`/login` API-key panel|`/login\r`, choose `openai`, type `sk-live-test\r`|The panel closes when `account/login/completed` arrives. `/tmp/dg-dal/fakehome` stays empty, and `/tmp/dg-dal/dal/auth.json` exists with an `openai` entry. The key check reaches `http://127.0.0.1:18080/v1/models`, so no network request leaves the host. That check adds nothing to `requests.jsonl`, because the server records chat requests only.|
|T20|Every other slash command|`/new`, `/fork`, `/compact`, `/revert`, `/resume` and the rest listed by T17|Each opens a panel and is navigable with arrows, Enter and Esc. `/revert` asks for confirmation. Run mid-turn (during t-slow), `/new`, `/compact`, `/fork`, `/resume`, `/tree` and `/revert` are refused. The live row shows the notice, for example `/compact is unavailable while a turn is working; wait for it to finish or interrupt it first`, and the full text stays in history. On a thread that holds more content than the kept window (`compaction_keep_tokens`, 20000 tokens by default), `/compact` produces a `t-compact` record. The context gauge shrinks. The live thread holds less. On it `/compact` shows `compact failed: rpc error -32600: compaction: nothing to summarize` and writes no record, which is the expected result there. `TestOverflowPrunesCompactsAndRetriesOnce` in `internal/dal/agent` checks the compaction and the refusal.|
|T21|Multi-line editing|`first line`, `\x0a` (ctrl+j), `second line\r`|The composer shows two lines. The `t-multi` record holds `first line\nsecond line`.|
|T22|History|`\e[A` on an empty composer|The last prompt comes back.|
|T23|`@file` completion|`@no\t`|Completes to `@notes.md`.|
|T24|Paste chip|bracketed paste of 30 lines with first line `PASTE-MARK-30`, then `\r`|The composer shows the chip `[Paste #1 +30 lines]`, and the `t-paste` record holds all 30 lines.|
|T25|Image paste|bracketed paste of `/tmp/dg-dal/ws/pixel.png`, then `describe the image\r`|The composer shows the chip `[Image #1 1x1]` ([tui.md](tui.md#editor)). The `t-image` record holds the image part that T46 checks.|
|T26|`!` shell mode|`!ls\r`|The output lists `a.txt` and `notes.md`, and no record is added. The stub log holds no `unmatched` line.|
|T27|A prompt that starts with `/`|`//etc/hosts is a path\r`, where a prompt that starts with `//` is sent with one slash removed ([tui.md](tui.md#keys-and-commands))|The record's user text starts with `/etc/hosts`.|
|T28|Mouse|in review: `\e[<64;10;5M`|The review scrolls. A click selects nothing ([tui.md](tui.md#mouse)): `\e[<0;10;5M\e[<0;10;5m` leaves the review and the dock unchanged.|
|T29|Rewind gesture|`\e` twice on an empty idle composer ([tui.md](tui.md#keys-and-commands)), then `\r` on the newest prompt and `\r` on the confirm|A rewind picker lists earlier prompts. Choosing one restores it into the composer.|
|T30|Notifications|in a session launched with `TERM=xterm-kitty` in its args: `\e[O`, `take your time\r`, wait 31 s|`raw` tail holds `\e]99;` (OSC 99). The `TERM` rule is in [tui.md](tui.md#title-and-notifications).|
|T31|Retry and terminal provider errors|`rate limit short\r`, `rate limit long\r`, `rate limit over\r`, `quota please\r`, `break the stream\r`, `drop the stream\r`|(1) Two records about 2 s apart (`t-429`, then `t-429-ok`), then `Recovered.`. (2) With the client attached, `rate limit long` (Retry-After 120) shows `rate limited · retrying in 2m 00s · esc stops`, and the count drops each second. `\e` stops the wait at once ([tui.md](tui.md#keys-and-commands)), with one record. (3) `rate limit over` (Retry-After 301) stops at once with `rate limited for 301 s, over the 300 s wait limit`, and one record. (4) `quota please` is a terminal quota error, with one record. (5) and (6) the row reads `Failed`, and no retry record. Attendance means an agent update subscriber, not a visible terminal (`internal/dal/agent/retry_wait.go:9-21`). The worker installs `model.RetryWatch` with `model.WithRetryWatch` in `internal/dal/agent/turn.go`. The attended and unattended limits are in [providers.md](providers.md#retry-and-timeouts). Countdown updates use `dalgona/retry/wait` (`tracker.retryWait` in `internal/dal/rpc/serve_track.go`, handled by `applyTurnNotification` and `applyRetryWait` in `internal/dal/tui/apply.go`). Do not equate headless `dal -p "rate limit long" --json` with no subscriber. The no-subscriber limit is a separate test-only case, `TestRetryWaitStopsWithoutSubscriber` in `internal/dal/agent/retry_wait_test.go:52-61`. Source wiring and test definitions do not prove this live row. Capture the observations above.|
|T32|Effort and `ultrathink`|`ultrathink about it\r`|The effort icon in the status band shows the raised level for this turn only (`DESIGN.md`, Icons glyph for that level). The `t-ultra` record carries the effort field the chatcompat encoder sends, absent or lower on the next turn. (`reasoning_effort`, sent only when the effort is set and is not `default`: `internal/dal/model/chatcompat/chatcompat.go:121-123`). The expected value is the level the status band shows for this turn. On the next turn, expect the configured baseline effort, or no `reasoning_effort` key if the baseline is empty or `default`.|
|T33|Six theme settings, auto detection, `NO_COLOR`, `DAL_NO_MOTION`|`/settings`, `down`, `down`, then `right` steps the theme through `auto`, `auto-soft`, `day`, `soft-day`, `night` and `soft-night` (config key `theme`, string, default `auto`, example `theme = "soft-night"`). Then sessions `dg-nocolor` and `dg-nomotion`|`shot` at each step while the panel is open (the preview applies at once), after t-md. With a dark emulator background, expect `auto` to resolve to `night` and `auto-soft` to `soft-night` (`DESIGN.md`, Colour). Check the installed external tool's background rather than relying on a line number in `tui.ts`. In every theme, no cell outside the point carries a background sequence. The `raw` bytes for those cells hold no `\e[48;`. The point is the only filled cell (`DESIGN.md`, The point and the thread hue). `dg-nocolor`: no colour sequence reaches `raw`. Inspect each SGR parameter. No parameter sits in 30 to 38, 40 to 48, 90 to 97 or 100 to 107. The point is reverse video (`\e[7m`), and state words remain. `dg-nomotion` during t-slow: `raw` with `clear: true`, wait 2 s, `raw` again: the dot stays static and writes nothing. Only the elapsed field updates, once per second, as T45 requires.|
|T34|Exit line, then the `dal` run|`\x03\x03`|The exit line reads `Thread <title> saved · resume with <product> --resume <id>` ([tui.md](tui.md#entry-point)), with `dalgona` or `dal` as the product. Then repeat T01 to T12 on session `dal` and check the `t-*` records again.|
|T35|Narrow mid-turn, empty composer|`take your time\r`, then `resize` to `cols: 40` during the turn, then `resize` back to 100|At 100 columns the live row shows `Working` and the elapsed time. At 40 columns the status band keeps the approval glyph `?` and the model name cut with `...`. No line is wider than 39 cells. No row of the old dock stays above the new dock. `screen` with `peek: 300` shows the `t-slow` text once.|
|T36|Narrow with one long line in the composer|send 120 `x` characters without Enter, then `resize` to `cols: 40`|The composer wraps to rows of at most 39 cells, and the cursor stays on the row that holds the insertion point. No dock row is wider than 39 cells. The terminal may rewrap rows of the earlier, wider dock above the cursor. Those rows are terminal history, not dock rows, so they are not judged. `screen` with `peek: 300` shows no history row twice.|
|T37|Narrow with three composer lines|`first`, `\x0a`, `second`, `\x0a`, `third`, then `resize` to `cols: 40`|The composer shows three lines, and the cursor stays on its line. The dock below the cursor is complete, and each dock row is at most 39 cells. Rows above the cursor line that the terminal rewrapped are terminal history. The check reads the rows of the dock and the typed lines only. `screen` with `peek: 300` shows no history row twice.|
|T38|Open and close each panel|`/settings\r` then `\e`. `/model\r` then `\e`. `/login\r` then `\e`. `/resume\r` then `\e`. `approve a long command\r`, then `n`. `ask me\r`, then `\e`. `\x0f`, then `\e`|Each panel opens between the composer and the live row ([tui.md](tui.md#layout)). The dock is taller only while a panel is open. After each close, `screen` shows the earlier dock height, and the rows above the dock match the capture taken before the panel opened.|
|T39|Stream a long answer|`show markdown\r` (t-md), then `screen` with `peek: 300` every 0.5 s until `Done`|Each capture holds the same rows above the dock as the earlier captures, from the first row that was written. Only the live tail row and the dock change during the stream. The status band keeps its row number in every capture ([tui.md](tui.md#layout): the dock height does not change while the window is the same).|
|T40|Suspend and resume|`take your time\r`, then `\x1a` (ctrl+z) during the turn. Resume from a second shell with `kill -CONT $(pgrep -f /tmp/dg-dal/bin/dal)`, because the tool has no job control|After `\x1a` the screen shows one blank row. After the resume the dock is drawn once at the cursor's line, with the `Working` tag and the elapsed time. `peek: 300` holds one copy of each history row.|
|T41|Quit at 40 columns, empty composer|`resize` to `cols: 40`, then `\x03\x03`|The exit row prints once, and the dock is gone. The shell prompt sits below the exit row. Nothing of the dock is printed to the history: the program commits only the committed rows and the exit row. Terminal rewrap of the earlier dock rows is not judged. No line follows the exit row except the shell prompt. A daemon log line that reaches the terminal after the exit row is a failure.|
|T42|Resize while streaming, 100x24 to 40x8 and back (dock bounds and history preservation)|start with `rows: 24`, send `take your time\r`, and during the turn `resize` to `cols: 40, rows: 8`, then `resize` to `cols: 100, rows: 24`|At 8 rows the status band is the last row, and every line is at most 39 cells. No row of the old dock stays above the new dock. After the resize back, `screen` with `peek: 300` shows each history row once, and the dock is complete.|
|T43|300-line tool output (print once, stable dock height)|`count to 300\r`, then `y`|At medium the scrollback holds `291` to `300` and not `1`. `\x0f` opens the review with `1` to `300`. `screen` with `peek: 300` shows the `300` row once. The dock height is the same before and after the output ([tui.md](tui.md#layout)).|
|T44|No erase or home sequence|`raw` with `clear: true` before `show markdown\r`, read after `Done` (T39)|The captured bytes hold none of `\e[2J`, `\e[3J`, `\e[H` or `\ec`, because the alternate screen is off in that session.|
|T45|One tick writes only fixed fields|`take your time\r`, then `raw` with `clear: true`, wait 1 s, then `raw` again|Each write in the second capture is a cursor move to the live row, then the live dot, the state word or the counter. No other row is written. With `DAL_NO_MOTION=1` the dot writes nothing. The second capture holds one cursor move to the live row and the changed cells of the elapsed field, in columns 11 to 17. The byte count is greater than 0, and no write lands in columns 1 to 10.|
|T46|Image part on the wire|the T25 paste, then `describe the image\r`|The `t-image` record's last user message holds one `image_url` part with a data URL ([providers.md](providers.md#requests)). The part's data is the base64 bytes of `pixel.png`.|
|T47|No dock row in the tmux history ([tui.md](tui.md#print), [dock bounds](tui.md#layout))|in tmux 3.6 with `scroll-on-clear on`. Start the client as the command of a new window (`tmux new-window '<binary>'`). Open and close the model panel, send `hello\r`, and wait for the turn to end. Then run `tmux capture-pane -p -S -`|The captured history holds the welcome row, the user row and the reply rows once each. No line of the dock sits above the last screen: no composer line, no status line, no panel line.|
|T48|Print path in tmux: no dock row in the history|start the client as the command of a new window (`tmux new-window '<binary>'`), then `alt+q`, then `esc`, then `take your time\r`, wait for `Done`, then `tmux capture-pane -p -S -`|No dock row appears in the history. Each printed row appears once, in order.|

## Dalgona session

Dalgona runs every Dal session row first, then these browser rows over the dalgona fixture with its daemon, web server, and browser. Dal MUST NOT run these rows.

A row that says `open thread <name>` needs an open thread. The fresh load opens no thread. Press `Start thread` to open a new thread, or select an earlier thread in the thread list. `<name>` is the thread that you open.

The feature checks run once at 1280 px in `day`. The layout matrix runs every width (320, 768, 1280) in each of the four themes, chosen through the web theme setting. Each matrix cell takes a full-page screenshot and runs these probes:

- `document.documentElement.scrollWidth <= innerWidth`
- every button and field has `getBoundingClientRect().height >= 36` (`DESIGN.md`, Browser controls)
- exactly one element in the view carries the solid point fill (`DESIGN.md`, The point and the thread hue). The product mark does not count. The selector is `.point, button.primary`.
- no element with `getBoundingClientRect().right > innerWidth + 0.5` inside `.shell`
- `document.fonts.check('16px "Hanken Grotesk"') && document.fonts.check('16px "Bricolage Grotesque"')`

|#|Feature|Action|Proof|
|---|---|---|---|
|W01|Connect, token, empty states|load the URL|The idle state word is `Ready`. No row shows `Offline`. The transcript says `Pick a thread or start a new one`. No thread is open and no thread row has `aria-current`. The empty state shows the `Start thread` button. The page icon is the candy mark, `dalgona-small.svg` (`DESIGN.md`, Product marks), served at `/dalgona-small.svg`. The source asset exists at `web/static/dalgona-small.svg`, and `web/src/app.html:6-7` links the icons. The web build writes assets to `internal/dalgona/webui/dist` (`web/vite.config.ts:10-13`), which `internal/dalgona/webui/webui.go:148-160` serves. Asset presence is not proof of a built or served icon. Inspect the live response. `curl` without a token gets 401 (in `live-check-dalgona`).|
|W02|Live regions|load|`[aria-live]` nodes exist at load with empty text. No `role=log` sits on the streaming delta container.|
|W03|Streaming Markdown, thinking shown, sanitizing|open thread `<name>`, then send `show markdown`|Heading, list, code and table render. Thinking collapses. The hostile line renders as text. The document title reads `dalgona · <title> · Ready` before the turn and `dalgona · <title> · Working` during it. After the turn it reads `dalgona · <title> · Done`. The title holds no hostile text.|
|W04|Tool card, read|open thread `<name>`, then send `read a.txt`|The card reads `Done`. The `t-read-2` record exists.|
|W05|Status band (`DESIGN.md`, Status band, Icons, Product marks)|after W04|The margin column is right-aligned with three rows. The first holds `model` as text. The second holds the visible items from `effort approval sandbox folder queue`. Effort shows only when the model has at least two supported levels. Its value is the thread effort, or the model default when unset. The scripted `qwen-test` model advertises no effort levels, so its effort item is absent. The third holds `context mark`. Every icon has an `aria-label` that states its value in words, and the folder icon also has `title` and `aria-label` with the folder name. The icons draw Nerd code points from the subset font whatever `icons` holds. Effort, approval and sandbox come from the `DESIGN.md`, Icons table. The context gauge is U+E38D to U+E39B at index `round(f * 14)`. The percent sits beside the gauge, and its element computes `font-variant-numeric` containing `tabular-nums` (`DESIGN.md`, Type for the web). The mark is `dalgona-small.svg`, and product chrome does not contain the mascot label `고나`. This restriction does not apply to user or model transcript text, including the required Korean fixture text in W27 (`DESIGN.md`, Product marks). At 1280, 768 and 320 px, take a screenshot and review it. At 320 px the band drops whole items by rank (`DESIGN.md`, Status band). Record the items in the second row at each width, and check that no text is cut mid-word.|
|W06|Working tag, motion|open thread `<name>`, then send `take your time`|The `Working` tag is the view's point in the thread hue (`DESIGN.md`, The point and the thread hue). Its computed background colour equals the table fill for the hue index `fnv1a32(threadID) % 4` in the active theme. On a second thread with another hue index, it equals that index's fill. It has the breathing dot and a ticking counter (`document.getAnimations().length > 0`).|
|W07|Follow-up queue|during W06, send `and then this`|The queue item shows the Nerd glyph U+F051F (`md-timer_sand`) then the count `1` (`DESIGN.md`, Status band, Icons). The follow-up is sent after the turn.|
|W08|Interrupt|open thread `<name>`, then during a new t-slow, Interrupt|The state tag reads `Stopped`. Focus moves to the composer, not `body`.|
|W09|Approval dialog|open thread `<name>`, then send `approve a long command`|At 320 px and at 768 px the actions stay visible while the 50-line command scrolls (the actions are sticky). The approval-queue note is inside `.shell`. Decline uses the quiet style. The approve action is the view's point (`DESIGN.md`, The point and the thread hue). It is the only solid accent fill in the dialog. Its buttons are at least 36 px tall (`DESIGN.md`, Browser controls).|
|W10|File-change approval diff|open thread `<name>`, then send `edit notes`|The dialog shows the `notes.md` diff with no 16rem cap cutting `+new line`, and lists the file. In `night`, the added and removed text reach 4.5:1 against their row ground, computed from the page colours (`DESIGN.md`, Components).|
|W11|Plan checklist|open thread `<name>`, then send `make a plan`|The text states appear, and `aria-current="step"` is on the in-progress step.|
|W12|Subagent, child approvals|open thread `<name>`, then send `spawn a helper`|The child block appears in the open thread.|
|W13|Ask tool|open thread `<name>`, then send `ask me`|Option list with `Orange` first, descriptions, preview, multi-select and `Other`. No `<form>`, `<input type=password>` or `<img>` inside the panel. The answer reaches `t-ask-2`.|
|W14|Settings, model, login panels|open each|Each validates on submit, has keyboard navigation (Tab, arrows, Esc) and no dead end. The model list shows both models. Login with `openai` and `sk-live-test` closes the panel when `account/login/completed` arrives. The key check reaches the scripted server, so no request leaves the host.|
|W15|Retry and errors|open thread `<name>`, then send the six error prompts from T31|The same texts as T31, with the countdown for `rate limit long` (Esc stops it). The short 429 shows the two records of T31 (1).|
|W16|Reconnect exhausted|open thread `<name>`, send `take your time`, then send `and then this` while the turn runs, then `kill -TERM $(cat /tmp/dg-dalgona/daemon.pid)` and leave the daemon down. Wait for `Disconnected from dalgona.`, then click `Reconnect` once. Then start the daemon with the `live-up-dalgona` daemon line and click `Reconnect` again.|The header reads `Offline`. The status region shows the warning `Reconnecting to dalgona (attempt 1 of 3).` with `Reconnect`. Each failed attempt replaces it with `Reconnecting to dalgona (attempt 2 of 3).` and then `Reconnecting to dalgona (attempt 3 of 3).` The attempts start 1 s, 2 s and 4 s after the failure before them. After the third failure the warning goes. The alert region shows `Disconnected from dalgona.` with `Reconnect`. No fourth attempt starts. The visible follow-up queue still lists `and then this`. The first click, with the daemon down, shows `Unable to connect to dalgona: <detail>. Check that dalgona web is still running.` and starts no attempt. The second click resumes the open thread and starts no turn. The queue still lists `and then this` until a turn ends.|
|W17|Selected thread not colour-only|open two threads|The selected item has `aria-current` and a non-colour cue that stays visible under forced colours. Each thread row has an 8 px dot in its thread hue (`DESIGN.md`, The point and the thread hue). The dot's index is `fnv1a32(threadID) % 4`. Its colour is one of the four hues of the active theme. The selected row has a 2 px bar in the `text` role on its leading edge, as well as the `tone` fill (`DESIGN.md`, Components).|
|W18|Jump to latest|open thread `<name>`, then send `long output`, scroll up 400 px|A jump-to-latest control appears. Clicking it brings the end into view.|
|W19|Sidebar cap at narrow widths|768 and 320 px|The thread list's own scroll box is capped at 18rem, not its wrapper.|
|W20|Context percent|after W04, open thread `<name>`|The `context` item's percent changes after `thread/tokenUsage/updated`. No token count is shown ([web.md](../../dalgona/spec/web.md#thread-header-and-status-band)).|
|W21|Edit review (opt-in)|open thread `<name>`, enable it in settings, `edit notes` with `t-judge` and `t-judge-turn`|The file change shows `Flagged: removed code`, the terminal shows a `review` row with that text, and the working model's second request body contains `<edit-review call=`. With the setting off, nothing appears. The judge is `local/qwen-test` through `[approval_decider]` ([config.md](config.md)). The judge call follows [edit-review.md](edit-review.md#judging-the-edit-unit).|
|W21b|Turn review (opt-in)|Open thread `<name>`, enable review, then send `edit notes` with `t-judge`, `t-edit-review-continue`, and `t-judge-turn`.|The feedback request matches `t-edit-review-continue` and receives a normal reply. Exactly one `t-judge-turn` request follows. One plain line after the turn's last message reads `No problems found` in dim text. With the setting off, no line appears. The judge is `local/qwen-test` through `[approval_decider]`. The judge call follows [edit-review.md](edit-review.md#turn-review). Judge routes precede the feedback route. Unrelated feedback has no fallback.|
|W22|Ratchet (dalgona)|Open thread `<name>`, then send `start a ratchet` through `t-ratchet`, `t-ratchet-2`, and `t-ratchet-approved`.|Init uses `a.txt` as cases, `src/main.go` as harness, and `notes.md` as change paths. These paths do not overlap. The next direct call approves inputs. The ratchet approval question opens the ask panel. The card shows `Approve`, `Revise`, and `Abort`. Select `Approve`. The final scripted reply confirms receipt of the approval result.|
|W23|Sandbox unavailable notice|run only when the report header says Landlock is unavailable, and open thread `<name>` first|Run only on a host without Landlock. The choice is asked once per thread. A second command in the same thread does not ask again, and the answer holds for that thread.|
|W24|Forced colours|the repo gate `web/checks/forced-colors.js` at 1280 and 320 px ([web.md](../../dalgona/spec/web.md#accessibility))|After each Tab press, the focused element's computed outline is `solid`, 2 px wide, with a 2 px `outline-offset`, in the normal themes. Under forced colours `outlineStyle` is not `none`. State tags keep their words. Borders mark the cards. Mark the report row `Proven by test only (web/checks/forced-colors.js)` only after executing the gate and retaining its passing output.|
|W25|Reduced motion|open thread `<name>`, then `reducedMotion: true` during t-slow|`document.getAnimations().length === 0`, and the counter holds still across 2 s.|
|W26|Themes|the 3x4 matrix|12 screenshots. Tags, buttons and status band follow `DESIGN.md` tokens. No gradient appears: no computed `background-image` contains `gradient`. The matrix uses the four concrete themes. `auto` gives `night` under dark and `day` under light. `auto-soft` gives `soft-night` under dark and `soft-day` under light. The page background (the `canvas` role) equals `#111113`, `#212225`, `#FFFCF0` and `#E6E4D9` for night, soft-night, day and soft-day (`DESIGN.md`, Colour).|
|W27|Hangul and fonts|open thread `<name>`, then t-md|The Hangul line renders in Pretendard: `document.fonts.check('16px Pretendard', '달')`.|
|W28|Focus after removed controls|approve or decline, then close panels|`document.activeElement !== document.body` after each.|
|W29|Seq guard and schema validation|Vitest only|Run the sequence-guard tests required by [web.md](../../dalgona/spec/web.md#thread-header-and-status-band) and the schema-validation tests required by [web.md](../../dalgona/spec/web.md#browser-client). The live server cannot reorder messages. Record test-only proof only after running the relevant tests and retaining their output.|
|W30|Codex methods|from the browser console over the open socket: `fuzzyFileSearch`, `mcpServerStatus/list`, `review/start`, `command/exec`, `config/batchWrite`, `fs/*`|Each answers a result, not "method not found".|
|W31|Automatic reconnect|open thread `<name>`, send `take your time`, then send `and then this` while the turn runs, then `kill -TERM $(cat /tmp/dg-dalgona/daemon.pid)`. At once start the daemon with the `live-up-dalgona` daemon line, before the third attempt starts. After the page connects, wait 20 s, kill the daemon again, then start it again.|The header reads `Offline`. The status region shows the warning `Reconnecting to dalgona (attempt <n> of 3).` with `Reconnect`. An attempt connects with no click. The warning goes and the header drops `Offline`. The open thread resumes and the reconnect starts no turn. The visible follow-up queue still lists `and then this` until a turn ends. The second kill shows `Reconnecting to dalgona (attempt 1 of 3).`, because 20 s of connection reset the count.|

## Defect loop

1. Freeze the evidence: the `screen` or screenshot, `raw` tail, the matching `requests.jsonl` lines, the product `llmstub.log`, and the check id. Add `/tmp/dg-dalgona/daemon.stderr` for a daemon row.
2. Name the contract the observation breaks: a sentence in `docs/dal/spec/<topic>.md` or `docs/dalgona/spec/<topic>.md`, `DESIGN.md`, or the Codex schema in `internal/dal/rpc/testdata/codex-schema`. If no contract covers it, write the spec sentence first in the same commit.
3. Pick the lowest layer that shows the defect:
   - Provider framing: a test in `internal/dal/model/chatcompat` built on `internal/llmstub`.
   - Protocol data: `internal/dal/rpc` serve tests.
   - Terminal render or keys: `internal/dal/tui` (the whole-program pattern in `internal/dal/tui/run_test.go`, with `Width` and `Height` set to 40 or 100).
   - Web: a Vitest file next to the component under `web/src`.
   - Daemon or recipe wiring: `cmd/dalgona` tests.
4. Write that test so its expected value comes from the contract named in step 2 of this loop. Run it on HEAD and record the failure line: `go test -run <Name> ./<pkg>` or `pnpm -C web exec vitest run <file>`.
5. Fix, rerun the test, rerun the live check id, then `just verify`. One commit holds the test, the fix and the spec line.
6. Add any script turn the defect needed to the product fixture, `internal/dal/e2e/testdata/live.json` or `internal/dalgona/e2e/testdata/live.json`, in the same commit. `TestLiveScriptParses` in each end-to-end package parses that fixture and runs the script validators. `TestLiveScriptRoutesScriptedRequests` loads and parses that file, then checks representative request routing. `TestLiveSubagentTurnYields` also loads it and checks the subagent's yield step. None of these tests executes the manual matrix.

The dal package checks the missing direction only: every turn that a Dal session row names exists in the dal fixture. The dalgona package checks both directions: every turn that a Dalgona row names exists in the dalgona fixture, and every fixture turn has a row. The tests are not behavioural proof of any row. `TestLiveSessionDriftNamesMissingAndUnrowedTurns` is the negative control in `internal/llmstub`. It shows that the check reports a missing turn and a turn with no row.

`TestLiveWorkspaceFailOutputIsExecutable` and `TestLiveWorkspaceFailOutputFollowsTheT12Contract` in `internal/dal/e2e` check the fixture script that row T12 runs.
`TestLiveModelListAnswersTheOpenAIKeyCheck` in `internal/dalgona/e2e` checks that the `openai` adapter reads the scripted model list with the `/login` key. The adapter gets 401 for another key.
The recipe wiring of that key stays a live check on T19 and W14.

## Not provable here

What cannot be proven here:

- Real provider wire formats, OAuth and device-code login against a real provider: needs credentials.
- `dalgona service install` starting at login: needs reboot.
- `dalgona codex install|status` and the app-server under the real app: needs the Codex desktop app.
- `--remote`, `cloud up|down|pull` and remote runners: needs a cloud service.
- Windows pinned writes, ConPTY and the macOS peer check: need another host.
- The sandbox-unavailable notice (W23) on a host where Landlock works: needs another host.
- Nerd glyph switch from kitty's font reply (`DESIGN.md`, Icons): needs another host (kitty with a Nerd Font).
- Clipboard image paste through the OS clipboard: needs a desktop session.
- How real terminals display notifications.
- Whether a real decision model judges well.

The report marks each with the matching `Not proven` status.

## Report

`/tmp/dg-dal/report.md` and `/tmp/dg-dalgona/report.md` hold one row per check id with the columns check, status, evidence and note. The status is one of:

- `Proven live`
- `Proven by test only (<package>.<Test>)`
- `Proven by test only (web/checks/forced-colors.js)`
- `Failed, defect <test name>`
- `Not proven: needs credentials`
- `Not proven: needs reboot`
- `Not proven: needs Codex desktop app`
- `Not proven: needs cloud service`
- `Not proven: needs another host (<what>)`
- `Not proven: no tui tool`
- `Not proven: needs external tool (<what>)`
- `Not proven: not run`
- `Pending local: browser run`
- `Pending local: PTY run`
- `Pending local: scripted-stack run`
- `Pending local: test run`

A row is never marked proven without evidence. The report starts with the HEAD commit, the binary revision, the host kernel, and whether Landlock was available.
The binary revision holds the `vcs.revision` and `vcs.modified` values that `go version -m` prints for the product binary, so a build from a modified tree is visible.

Behavioural acceptance MUST record a real client action, a contract-based observation, a negative control, and the source and binary revision. A source file, a test declaration, a probe file, a recipe, or a documentation artifact is not a passing observation.

Keep the evidence axes separate for each row. Name the inspected source or test artifact, the contract, and its positive and negative controls. Name the observed execution with its run provenance, and any genuine external prerequisite. Preserve the original failure receipts and the partial-run limits after a fix.

A runnable local check with no recorded observation holds a pending-local status.
