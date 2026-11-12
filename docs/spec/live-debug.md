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
`just live-up` MUST write one configuration for each product. The `dalgona` configuration MUST set `[edit_review] feedback` to `lost_code`.
`just live-up` MUST start the scripted server on `127.0.0.1:18080` and record its requests to `/tmp/dg/requests.jsonl`.
`just live-up` MUST start the daemon and `dalgona web` with isolated homes.
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
