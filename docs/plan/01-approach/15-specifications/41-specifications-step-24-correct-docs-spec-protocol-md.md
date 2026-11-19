#### Specifications step 24. Correct `docs/spec/protocol.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/protocol.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 16 | Invalid request. Valid JSON that is not a request, for example wrong member types, and thread state conflicts | Invalid request. Valid JSON whose envelope is not a request, for example a non-string `method`, and thread state conflicts | internal/rpc/conn.go:195-198 `if err := json.Unmarshal(raw, &e); err != nil {` / `if json.Unmarshal(raw, &v) == nil {` / `c.replyInvalidRequest(raw)`; conn.go:71 `Method string` in envelope |
| 18 | Invalid or unknown parameters, for example `unknown thread <id>` | Invalid or unknown parameters, for example a wrong parameter member type or `unknown thread <id>` | internal/rpc/serve_wire.go `decodeParams`: `return invalidParams(err.Error())` when `readMembers` fails |
| 22 | The writer queue holds 1024 messages. | The writer queue MUST hold at most 4096 messages. Delta notifications MAY merge only while the queue holds 1024 or more messages. | internal/rpc/conn.go:567 `if len(q.items) < 1024 \|\| item.stream == nil {`; conn.go:589 `if len(q.items) >= 4096 {` |
| 22 | A connection with 4096 queued non-delta messages MUST close and log `rpc: closing slow client <addr>`. | A connection with 4096 queued messages, deltas included, MUST close and log `rpc: closing slow client <addr>`. | internal/rpc/conn.go:589 `if len(q.items) >= 4096 {` counts every queued item; conn.go:430 `slog.Error("rpc: closing slow client " + c.remote)` |
| 28 | The server encodes every message with `encoding/json/jsontext`. | The server encodes every message with `encoding/json/jsontext` or `encoding/json/v2`. | internal/rpc/conn.go:8 `json "encoding/json/v2"`; conn.go:183 `return json.Marshal(v)` in marshalParams; serve_encode.go:3 `import "encoding/json/jsontext"` |
| 30 | ## Session start | ## Startup | internal/rpc/serve.go:79 `func Serve(ctx context.Context, conn io.ReadWriteCloser, host *agent.Host) error {`; the section covers the per-connection initialize and daemon-start recovery (protocol.md:32-34) |
| 40 | It is journaled with the session | It is journaled with the thread | internal/agent/host.go:142 `os.Stat(filepath.Join(h.cfg.Config.Home, "sessions", head.ID, "journal.jsonl"))`: the thread journal lives under the thread ID; host.go:138-139 `agent: session %s has no session.created` takes the thread head ID |
| 78 | Decisions `accept`, `acceptForSession`, `decline`, and `cancel` map to the host replies. | Decisions `accept`, `acceptForSession`, `decline`, and `cancel` map to the host replies. The decision `approved` maps to accept, `approved_for_session` maps to acceptForSession, and `abort` maps to cancel. | internal/rpc/serve_approval.go:170 `switch decision {`; :171 `case "accept", "approved":`; :173 `case "acceptForSession", "approved_for_session":`; :175 `case "cancel", "abort":`; :178 `return agent.ReplyDecline` |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 7 | The server MUST NOT accept a request line longer than 16 MiB. | internal/rpc/conn.go:187 `s := bufio.NewScanner(c)`; conn.go:188 `s.Buffer(make([]byte, 4096), 16<<20)` |

Use one word for one thing.

- Keep `thread; keep the literal event name session.created at line 72 (internal/agent/event/event.go:13)`. Replace session at lines 30, 40.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 68 | A product MAY run a precondition before `thread/archive` on an idle thread. If the precondition fails, the thread MUST stay unarchived. The server MUST return error `-32603` with the message `archive hook: <cause>`. |
| 72 | The effective sandbox is `off` when the configured policy is not `off` and the runner reports no harness sandbox. The server gets this value from the agent layer. |
| 76 | A workspace trust prompt reuses `item/commandExecution/requestApproval`. It sets `cwd` to the workspace path. It sets `command` to the server commands and env, joined by newlines. It sets `reason` to the full workspace prompt. |
| 78 | A client MUST handle each server request on its own task. An open prompt MUST NOT block notification delivery. A client MUST cancel that task when the matching `serverRequest/resolved` arrives. |
| 106 | The tool class sets the item kind. Class `exec` gives `commandExecution`. Class `edit` gives `fileChange` with one change per file. Class `remote` gives `mcpToolCall` with the tool name `<server>__<tool>`. Any other tool gives `dynamicToolCall`. |
| 106 | A call that completes without a start, with a `decline` or `cancel` decision, has status `declined` for `commandExecution` and `fileChange`. For other kinds, that call has status `failed`. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 9 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/protocol.md` prints `style findings: 0`, and `just verify` passes.

