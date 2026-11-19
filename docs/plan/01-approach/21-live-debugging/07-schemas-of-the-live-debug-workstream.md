#### Schemas of the live-debug workstream

##### Script file (`-script`, JSON)

| Field | Type | Rule |
|---|---|---|
| `models` | array of `{id: string, context_length: int, architecture: {input_modalities: string[]}}`, where `architecture` is optional and `input_modalities` takes `text` and `image` | 1 to 8 entries. `id` is non-empty and unique. `context_length` is 0 or more. |
| `turns` | array of Turn | 1 to 256 entries |
| Turn `id` | string | `^[a-z0-9-]{1,40}$`, unique |
| Turn `when` | object with exactly one of `user`, `tool_result`, `any_text` (string, 1 to 200 bytes) | `user` matches when the last message has role `user` and its text contains the value. `tool_result` matches when the last message has role `tool` and `tool_call_id` equals the value. `any_text` matches when any message text contains the value. |
| Turn `once` | bool, default false | A spent `once` turn never matches again. |
| Turn `status` | int, 0 or 400 to 599, default 0 | When it is not 0: `steps` is empty, `body` is required, and the reply is that status with `Content-Type: application/json`. |
| Turn `retry_after` | string of digits, 1 to 5 characters | Only with `status` 429 or 503. Sent as the `Retry-After` header. |
| Turn `body` | JSON value | Only with `status`. Sent verbatim. |
| Turn `steps` | array of Step, 1 to 512 | The last step is `finish`, `error_frame` or `drop`. No step follows `drop`. `finish` may appear only last. |
| Step | object with exactly one of the keys below | |
| `text` | string | Frame `{"choices":[{"index":0,"delta":{"content":<text>}}]}` |
| `reasoning` | string | Frame `{"choices":[{"index":0,"delta":{"reasoning_content":<text>}}]}` |
| `tool_call` | `{id: string, name: string, arguments: JSON object, chunks: int 1..16 (default 1)}` | First chunk: `{"choices":[{"index":0,"delta":{"tool_calls":[{"index":<k>,"id":<id>,"type":"function","function":{"name":<name>,"arguments":<chunk 1 as a JSON string>}}]}}]}`. Later chunks carry only `index` and `function.arguments`. `k` counts the tool calls in this turn from 0. |
| `delay_ms` | int 0..120000 | Waits; ends early when the client cancels. |
| `finish` | `"stop"`, `"length"` or `"tool_calls"` | Frame `{"choices":[{"index":0,"delta":{},"finish_reason":<v>}],"usage":{"prompt_tokens":p,"completion_tokens":c}}`. `usage` is taken from the sibling key `usage` on the same step object, default 1/1. This is the one step that allows a second key, and only `usage`. |
| `error_frame` | `{code: string, message: string}` | Frame `{"error":{"code":..,"message":..}}` (`decode.go:107-108`) |
| `drop` | `true` | Closes the connection with no `[DONE]` (oracle: `internal/model/chatcompat/decode.go:263-265`, "chatcompat: stream ended without a finish reason") |

After `finish` or `error_frame` the stub writes `data: [DONE]\n\n` (`decode.go:93-94`). Every frame is `data: <compact JSON>\n\n` with no `event:` line, sent with `Content-Type: text/event-stream` and `Cache-Control: no-cache`, and flushed after each frame.

Example turn:

```json
{"id":"t-read","when":{"user":"read a.txt"},"steps":[{"reasoning":"Look at the file."},{"text":"checking"},{"tool_call":{"id":"call-read","name":"read","arguments":{"path":"a.txt"},"chunks":3}},{"finish":"tool_calls","usage":{"prompt_tokens":1200,"completion_tokens":40}}]}
```

##### Record line (`-record`, JSONL)

`{"seq": int from 1, "path": string, "turn": string (turn id or "unmatched"), "body": the request JSON verbatim}`

Example:

```json
{"seq":2,"path":"/v1/chat/completions","turn":"t-read-2","body":{"model":"qwen-test","messages":[...]}}
```

##### Live script `internal/llmstub/testdata/live.json`

`models`: `[{"id":"qwen-test","context_length":32000,"architecture":{"input_modalities":["text","image"]}},{"id":"qwen-small","context_length":8000}]`. The second model gives `/model` and the presets a real choice.

Turns. The prompt is the `when.user` text unless noted. Tool names are cited from `internal/tool/*/`.

| id | when | steps |
|---|---|---|
| `t-md` | `show markdown` | reasoning `Planning the answer.` as 3 reasoning steps with `delay_ms` 300 between them. Text: a `## Heading`, a 3-item list, a fenced `go` block of 8 lines, a 3x3 table, one 200-character line, one Hangul line `달고나 테스트 문장입니다`, and the hostile line `\u001b]0;pwned\u0007\u001b[2Jafter`. Finish `stop`, usage 1200/340. |
| `t-read` / `t-read-2` | `read a.txt` / tool_result `call-read` | the example above / text `a.txt says hello.`, finish `stop`, 1300/12 |
| `t-exec` / `t-exec-2` | `run the counter` / tool_result `call-exec` | `exec` (`internal/tool/exec/exec.go:30`) with `{"command":"seq 1 40"}`, finish `tool_calls` / text `40 lines.`, finish `stop` |
| `t-exec-fail` / `t-exec-fail-2` | `run a failing command` / tool_result `call-fail` | `exec` with `{"command":"sh -c 'seq 1 30; echo boom >&2; exit 3'"}` / text `It failed with exit 3.`, finish `stop` |
| `t-long-cmd` / `t-long-cmd-2` | `approve a long command` / tool_result `call-long` | `exec` whose `command` is 50 lines (`seq 1 50 | sed 's/^/line /'`) / text `ok`, finish `stop` |
| `t-patch` / `t-patch-2` | `edit notes` / tool_result `call-patch` | `apply_patch` changing `notes.md` line `old line` to `new line`. The argument is `{"input": "*** Begin Patch\n*** Update File: notes.md\n@@\n-old line\n+new line\n*** End Patch\n"}`, the one required field of the `apply_patch` schema (`internal/tool/patch/description.go:40`). It needs `edit_style = "apply_patch"`. / text `Edited notes.md.`, finish `stop` |
| `t-plan` / `t-plan-2` | `make a plan` / tool_result `call-plan` | `eval` with `{"code": "tools.todo(items = [{'text': 'Read the code', 'status': 'done'}, {'text': 'Write the fix', 'status': 'in_progress'}, {'text': 'Run the tests', 'status': 'pending'}])"}`. The cell calls `todo` (`internal/tool/todo/todo.go:25-27`), which `eval-first` folds / text `Plan set.`, finish `stop` |
| `t-sub` / `t-sub-child` / `t-sub-2` | `spawn a helper` / any_text `helper task: count files` / tool_result `call-sub` | `eval` with `{"code": "tools.task(prompt = 'helper task: count files')"}`. The cell calls the task tool, whose required `prompt` field is at `internal/tool/task/task.go:38`, and `eval-first` folds it. / text `There are 5 files.`, finish `stop` / text `The helper counted 5 files.`, finish `stop` |
| `t-ask` / `t-ask-2` | `ask me` / tool_result `call-ask` | the `ask` tool (the ask workstream, Step 1) with `{"questions": [{"header": "Colour", "question": "Which colour?", "options": [{"label": "Orange", "description": "Warm.", "preview": "orange"}, {"label": "Slate", "description": "Cool."}]}, {"header": "Files", "question": "Which files?", "multi_select": true, "options": [{"label": "a.txt", "description": "The notes file."}, {"label": "notes.md", "description": "The markdown file."}, {"label": "src/main.go", "description": "The program."}]}]}` / text `Thanks.`, finish `stop` |
| `t-slow` | `take your time` | text `Starting.`, `delay_ms` 30000, text `Finished.`, finish `stop` |
| `t-429` (once) / `t-429-ok` | `rate limit short` (both) | status 429, `retry_after` `"2"`, body `{"error":{"code":"rate_limit","message":"slow down"}}` / text `Recovered.`, finish `stop` |
| `t-429-long` | `rate limit long` | status 429, `retry_after` `"120"`, the same body |
| `t-429-over` | `rate limit over` | status 429, `retry_after` `"301"`, the same body |
| `t-402` | `quota please` | status 402, body `{"error":{"code":"insufficient_quota","message":"quota exhausted"}}` |
| `t-midfail` | `break the stream` | text `partial`, error_frame `server_error`/`upstream reset` |
| `t-drop` | `drop the stream` | text `partial`, drop |
| `t-ultra` | `ultrathink about it` | text `Thought hard.`, finish `stop` |
| `t-long` | `long output` | 400 text steps `line <n>\n`, with `delay_ms` 20 every 50 lines, finish `stop` |
| `t-compact` | any_text `ummar` | text `Summary: the user read a.txt and edited notes.md.`, finish `stop`, usage 900/30. The request is `internal/agent/compaction.go:194`, with `System: prompts.Compaction`, the text of `internal/agent/prompts/compaction.md` embedded by `internal/agent/prompts/assets.go:8-9`. Its line 4 holds `summary`, which `ummar` matches. `system.md` holds neither word. No title-generation request exists in `internal/`, so no title turn is scripted. |
| `t-judge` | any_text `"off_task"` | text `{"sound":0.04,"lost_code":0.97,"placeholder":0.02,"off_task":0.01}`, finish `stop`. Edit judge of a text edit (`notes.md`, task not empty). The run configuration (the section "Schemas of the live-debug workstream") sets `[approval_decider] decider = "llm:local/qwen-test"`, so this turn answers the edit-review judge. The reply is the flat object that the edit-review workstream names: one number for each question id asked, here `sound`, `lost_code`, `placeholder` and `off_task`, and no other key. |
| `t-judge-turn` | any_text `"incomplete"` | text `{"sound":0.04,"incomplete":0.03}`, finish `stop`. Turn judge, asked when the working model ends the turn after the edit. |
| `t-ratchet` / `t-ratchet-2` | `start a ratchet` / tool_result `call-ratchet` | the `ratchet` tool with `{"action": "init", "flow": "live", "cases": ["a.txt"], "harness": ["notes.md"], "change": ["notes.md"]}` (the ratchet workstream, Step 6, lists the members) / text `Ratchet round done.`, finish `stop` |
| `t-paste` | any_text `PASTE-MARK-30` | text `Got 30 lines.`, finish `stop` |
| `t-multi` | any_text `first line\nsecond line` | text `Got two lines.`, finish `stop` |
| `t-image` | `describe the image` | text `A single pixel.`, finish `stop` |
| `t-followup` | `and then this` | text `Noted.`, finish `stop` |
| `t-slash` | `/etc/hosts is a path` | text `Path noted.`, finish `stop` |
| `t-exec-300` / `t-exec-300-2` | `count to 300` / tool_result `call-exec300` | `exec` with `{"command": "seq 1 300"}`, finish `tool_calls` / text `300 lines.`, finish `stop` |

##### Run configuration (written by `live-up` to both homes)

```toml
model = "local/qwen-test"
approval = "ask"
edit_style = "apply_patch"

[approval_decider]
decider = "llm:local/qwen-test"

[providers.local]
base_url = "http://127.0.0.1:18080/v1"
api_key_env = "LOCAL_LLM_KEY"

[models."local/qwen-test"]
context_window = 32000
```

The `dalgona` home gets one more table, because `dal` rejects the key: `[edit_review]` with `feedback = ["lost_code"]`. It sets no `enabled` key, so the review stays off until scenario W21 turns it on in the settings. Without the `feedback` key no question speaks, and W21 would see no `Flagged: removed code` text.

The keys come from `internal/config/merge.go:24-26,37-38,153-158`. `approval = "ask"` is needed so approval cards appear. `edit_style = "apply_patch"` makes `apply_patch` the edit tool, because the default is `anchor` (`internal/config/config.go:261`). `[approval_decider]` points the edit-review judge at the scripted model. In `ask` mode the gate asks the human and never calls the decider (`internal/agent/gate.go:104-105`). Theme and verbosity are changed live through `/settings`. They are not preset.

##### justfile additions (exact)

```just
live_root := "/tmp/dg"

omp-tui-install:
    #!/usr/bin/env bash
    set -euo pipefail
    src=.references/oh-my-pi/.omp/tools
    for f in tui.ts package.json bun.lock; do
        test -f "$src/$f" || { echo "omp-tui-install: missing $src/$f" >&2; exit 1; }
    done
    command -v bun >/dev/null || { echo "omp-tui-install: bun is required" >&2; exit 1; }
    mkdir -p .omp/tools
    cp "$src/tui.ts" "$src/package.json" "$src/bun.lock" .omp/tools/
    (cd .omp/tools && bun install --frozen-lockfile)
    echo "omp-tui-install: run /restart in omp to load the tui tool"

live-up:
    #!/usr/bin/env bash
    set -euo pipefail
    w={{live_root}}
    if [ -e "$w/llmstub.pid" ] && kill -0 "$(cat "$w/llmstub.pid")" 2>/dev/null; then
        echo "live-up: $w is in use; run just live-down" >&2; exit 1
    fi
    rm -rf "$w"
    mkdir -p "$w/bin" "$w/dal" "$w/dgh" "$w/fakehome" "$w/ws"
    chmod 700 "$w/dal" "$w/dgh"
    go build -trimpath -o "$w/bin/" ./cmd/dal ./cmd/dalgona ./internal/llmstub/cmd/llmstub
    cp -R internal/llmstub/testdata/workspace/. "$w/ws/"
    git -C "$w/ws" init -q
    git -C "$w/ws" add -A
    git -C "$w/ws" -c user.name=live -c user.email=live@localhost commit -qm seed
    printf '%s\n' 'model = "local/qwen-test"' 'approval = "ask"' 'edit_style = "apply_patch"' '' '[approval_decider]' 'decider = "llm:local/qwen-test"' '' '[providers.local]' \
        'base_url = "http://127.0.0.1:18080/v1"' 'api_key_env = "LOCAL_LLM_KEY"' '' \
        '[models."local/qwen-test"]' 'context_window = 32000' > "$w/dal/config.toml"
    cp "$w/dal/config.toml" "$w/dgh/config.toml"
    printf '%s\n' '' '[edit_review]' 'feedback = ["lost_code"]' >> "$w/dgh/config.toml"
    printf 'export HOME=%s DAL_HOME=%s DALGONA_HOME=%s LOCAL_LLM_KEY=dummy\n' "$w/fakehome" "$w/dal" "$w/dgh" > "$w/env"
    base=(env -i PATH="$PATH" HOME="$w/fakehome" DAL_HOME="$w/dal" DALGONA_HOME="$w/dgh" LOCAL_LLM_KEY=dummy)
    nohup "$w/bin/llmstub" -listen 127.0.0.1:18080 -script internal/llmstub/testdata/live.json \
        -key dummy -record "$w/requests.jsonl" >/dev/null 2>"$w/llmstub.log" &
    echo $! > "$w/llmstub.pid"
    for _ in $(seq 50); do grep -q '^llmstub: serving ' "$w/llmstub.log" && break; sleep 0.1; done
    grep -qx 'llmstub: serving http://127.0.0.1:18080/v1' "$w/llmstub.log" || { echo "live-up: llmstub did not start; see $w/llmstub.log" >&2; exit 1; }
    "${base[@]}" nohup "$w/bin/dalgona" daemon >/dev/null 2>"$w/daemon.stderr" &
    echo $! > "$w/daemon.pid"
    for _ in $(seq 50); do test -S "$w/dgh/dalgona.sock" && break; sleep 0.1; done
    test -S "$w/dgh/dalgona.sock" || { echo "live-up: daemon did not start; see $w/daemon.stderr" >&2; exit 1; }
    "${base[@]}" nohup "$w/bin/dalgona" web >/dev/null 2>"$w/web.log" &
    echo $! > "$w/web.pid"
    re='^http://127\.0\.0\.1:[0-9]+/#token=[A-Za-z0-9_-]+$'
    for _ in $(seq 100); do grep -Eq "$re" "$w/web.log" && break; sleep 0.1; done
    grep -E "$re" "$w/web.log" > "$w/web.url" || { echo "live-up: web did not start; see $w/web.log" >&2; exit 1; }
    echo "live-up: env $w/env; web $(cat "$w/web.url")"

live-down:
    #!/usr/bin/env bash
    set -uo pipefail
    w={{live_root}}
    status=0
    for name in web daemon llmstub; do
        f="$w/$name.pid"; [ -e "$f" ] || continue
        pid=$(cat "$f")
        cmd=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null || true)
        case "$cmd" in "$w/bin/"*) kill -TERM "$pid" ;; *) rm -f "$f"; continue ;; esac
        for _ in $(seq 60); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
        if kill -0 "$pid" 2>/dev/null; then echo "live-down: $name pid $pid still running after 6 s" >&2; status=1; else rm -f "$f"; fi
    done
    pkill -TERM -x -f "$w/bin/dalgona daemon" || true
    tmux -L dglive kill-server 2>/dev/null || true
    for _ in $(seq 50); do test -S "$w/dgh/dalgona.sock" || break; sleep 0.1; done
    if test -S "$w/dgh/dalgona.sock"; then echo "live-down: $w/dgh/dalgona.sock remains" >&2; status=1; fi
    exit $status

live-check:
    #!/usr/bin/env bash
    set -euo pipefail
    w={{live_root}}
    just live-up
    trap 'just live-down' EXIT
    source "$w/env"
    cd "$w/ws"
    "$w/bin/dal" -p "read a.txt" --json > "$w/check-dal.json"
    "$w/bin/dalgona" -p "read a.txt" --json > "$w/check-dalgona.json"
    test "$(grep -c '"turn":"t-read-2"' "$w/requests.jsonl")" = 2
    test "$(grep -c 'hello from a.txt' "$w/requests.jsonl")" -ge 2
    ! grep -q '"turn":"unmatched"' "$w/requests.jsonl"
    base=$(sed 's|/#token=.*||' "$w/web.url")
    test "$(curl -s -o /dev/null -w '%{http_code}' "$base/rpc")" = 401
    test "$(curl -s -o /dev/null -w '%{http_code}' "$base/")" = 200 || { echo "live-check: web UI not built; run just web" >&2; exit 1; }
    echo "live-check: ok"
```

The socket path `/tmp/dg/dgh/dalgona.sock` is 24 bytes, under the 108-byte Linux limit (`docs/spec/daemon.md` "daemon socket path"). `HOME` points at an empty fake home, so discovery never reads the developer's `~/.codex` or `~/.claude`.

