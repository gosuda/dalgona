#### Specifications step 12. Correct `docs/spec/exec-server.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/exec-server.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 33 | The floor is `workspace-write` only when the server itself started with `DALGONA_SANDBOX_MODE=workspace-write`; otherwise the box is the sandbox and `off` stays allowed. | The floor is `off` when `DALGONA_SANDBOX_MODE` is empty or `off`. Any other value sets the floor to `workspace-write`. When the floor is `off`, the box is the sandbox, so `process/start` MAY use mode `off`. | internal/runner/execserver.go:175-178 `case SandboxOff, "":` / `return SandboxOff` / `default:` / `return SandboxWorkspaceWrite`; wire.go:145-147 `if floor == SandboxOff { return nil }` |
| 44 | On Linux, when `DALGONA_SYNCFS` names a directory, the server MUST call `syncfs` on that directory after the rename completes. | On Linux, when `DALGONA_SYNCFS` names a directory, the server MUST call `syncfs` on that directory after each write completes. | internal/runner/wire.go:280 `if syncErr := syncFS(); syncErr != nil {` inside rpcWriteFile (wire.go:268-283), which runs after both the offset-zero and nonzero-offset branches |
| 62 | A wrong token MUST receive HTTP 401. Other paths MUST receive HTTP 404. | A request with a missing or wrong token MUST receive HTTP 401 on every path. A request with the right token to another path MUST receive HTTP 404. | internal/runner/execserver.go:191-196 `if !bearerAuthorized(...) {` / `http.Error(w, "unauthorized", http.StatusUnauthorized)` runs before `routeExecWS` (execserver.go:196); `http.NotFound` only at execserver.go:211; execserver_test.go:55-56 pins 401 on path " |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 54 | The `--root` flag defaults to `/workspace`. | internal/runner/execserver.go:71 `root := flags.String("root", "/workspace", "workspace root")` |
| 54 | An invalid command line MUST exit with status 2. | internal/runner/execserver.go:51-52 `_, _ = fmt.Fprintln(stdout, err)` / `return 2` |
| 54 | A listen address other than `stdio://` or `ws://IP:PORT` MUST fail with `exec-server: listen must be stdio:// or ws://IP:PORT`. | internal/runner/execserver.go:80-81 `return execServerConfig{}, errors.New("exec-server: listen must be stdio:// or ws://IP:PORT")` |
| 64 | The box channel is the `/` path. | internal/runner/execserver.go:202-203 `case "/":` / `serveExecWebsocket(ctx, w, r, local, nil)`; execserver.go:255-256 `} else {` / `err = serveBoxChannel(ctx, conn, local, serverSandboxFloor())` |
| 64 | On cancellation, a websocket server MUST stop and wait at most 5 s for shutdown. | internal/runner/execserver.go:222-223 `case <-ctx.Done():` / `return shutdownExecListener(server, errCh)`; execserver.go:228 `context.WithTimeout(context.Background(), 5*time.Second)` |

Structure:

- Line 33 uses the term websocket box channel before line 64 defines it (see add after line 64).
- Serve passes trustedExecPolicy() (execserver.go:41, wire.go:75), so the stdio path is not under the sandbox floor. The stdio branch of runExecServer (execserver.go:102-104) calls Serve, so the stdio path also uses trustedExecPolicy.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 15 | When `TERM` fails because the stream closed, the caller MUST open one fresh connection if the runner can redial. That connection MUST open within 2 s. Over that connection, the caller MUST send `TERM`, poll, and `KILL`. |
| 33 | A request on the websocket box channel MUST NOT weaken the sandbox below the server start mode. When the server floor is `workspace-write`, `process/start` with any other mode MUST fail with `-32602`. |
| 35 | The server MUST return at most `limit` bytes. The default `limit` is 1 MiB when `limit` is missing or out of range. That keeps each response line within the 16 MiB framing cap. |
| 44 | A nonzero `offset` MUST write one chunk of a paged upload at that byte position. The client MUST split large files into 1 MiB chunks. That keeps each request line within the 16 MiB framing cap. |
| 44 | The client MUST stage a multi-chunk upload in a temporary file in the destination directory. The client MUST publish that file with `fs/rename` only after the last chunk succeeds. |
| 44 | When the destination contains `/`, the staging directory ends after the last `/`. In that case, `\` is a name byte. Otherwise, when the destination contains `\`, the staging directory ends after the last `\`. Otherwise, the staging name stands alone. |
| 44 | The staging basename MUST be `.dalgona-partial-` followed by 16 lowercase hex digits. The client MUST take those digits from a cryptographic random source. The basename is 33 bytes in total, whatever the destination name length. |
| 44 | The basename MUST NOT reuse the destination name. So the staged file cannot shadow a user file. The `.dalgona-partial-` prefix MUST let a later cleanup recognize a file that a crashed upload left behind. |
| 44 | The removal MUST run under a context detached from the upload context. The removal MUST have a 5 s timeout. So a silent server cannot block the caller past that timeout. |
| 72 | The decode MUST land in a temporary file in the same directory. The install MUST be an atomic rename. A running detached binary never sees a truncated page. The write never hits `ETXTBSY`. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 12 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/exec-server.md` prints `style findings: 0`, and `just verify` passes.

