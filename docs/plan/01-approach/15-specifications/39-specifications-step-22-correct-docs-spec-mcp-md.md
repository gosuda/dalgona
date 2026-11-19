#### Specifications step 22. Correct `docs/spec/mcp.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/mcp.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 19 | The client MUST discard child standard error output. | The client MUST keep the last 4096 bytes of child standard error output. The client MUST include that tail in each stdio transport error. Tool output passes the secret mask before it enters the journal. | internal/mcp/stdio.go:30 `const stderrTailBytes = 4096`; internal/mcp/stdio.go:91 `cmd.Stderr = &t.stderr`; internal/mcp/stdio.go:163; internal/agent/tools.go:417 `maskedOutput, err := w.t.host.mask(output)` |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 37 | Closing the HTTP client MUST send an HTTP DELETE with `Mcp-Session-Id` when a session exists. The client MUST wait at most two seconds for that DELETE and MUST ignore its failure. | internal/mcp/http.go:225-227 `func (t *httpTransport) Close() error {` / `if t.sessionID != "" {` / `t.deleteSession()`; http.go:236 `context.WithTimeout(context.Background(), 2*time.Second)`; http.go:238 `http.NewReques |
| 37 | The error for a non-2xx response MUST contain at most the first 8192 bytes of the response body. | internal/mcp/http.go:126-127 `func readHTTPStatusError(response *http.Response) error {` / `body, err := io.ReadAll(io.LimitReader(response.Body, 8192))`; http.go:80 `return nil, readHTTPStatusError(response)` |
| 59 | A trust prompt that a client can answer MUST wait until the client answers or the request context ends. | internal/agent/workspace_trust.go:52 `select {`, :58 `return reply, nil`, :59 `case <-ctx.Done():`, :60 `t.discardApproval(requestID)`, :65 `return ReplyCancel, ctx.Err()` |

Structure:

- Workspace trust section: a reader cannot tell how long a trust prompt waits. Add the wait sentence after line 59 (see add).

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 57 | The host MUST ask once per workspace. The prompt MUST show the workspace path, every server command, its arguments, and its env keys. The host MUST send the prompt through the approval channel that asks about tool calls. |
| 57 | Env entries reach the child process. The prompt MUST show the `PATH` and `LD_PRELOAD` keys when set. Every env value MUST appear only as `KEY=<redacted>`. The summary, reason, commands, and RPC log MUST NOT contain a raw env value. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 2 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/mcp.md` prints `style findings: 0`, and `just verify` passes.

