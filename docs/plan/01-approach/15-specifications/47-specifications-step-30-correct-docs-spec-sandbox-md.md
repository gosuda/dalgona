#### Specifications step 30. Correct `docs/spec/sandbox.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/sandbox.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 38 | The shim MUST exec `/usr/bin/sandbox-exec -p <profile> -- argv...`. | On macOS, the runner MUST start the command as `/usr/bin/sandbox-exec -p <profile> -- argv...`. | internal/runner/sandbox_darwin.go:34-37 `args := []string{sandboxExec, "-p", sandboxProfile(roots, denied), "--"}` / `cmd.Path = sandboxExec`; sandbox_darwin.go:41-43 `func execSandboxed` returns `runner: unexpected nested shim on macOS`. |
| 41 | (version 1) (allow default) (deny network-outbound (remote unix-socket (path-literal "<denied>"))...) (deny file-write*) (allow file-write* (subpath "<root>")...) (allow file-write* (literal "/dev/null") (literal "/dev/zero") (regex #"^/dev/tty") (regex #"^/dev/fd/")) | (version 1) (allow default) (deny network-outbound (remote unix-socket (path-literal "<denied>")))... (deny file-write*) (allow file-write* (subpath "<root>"))... (allow file-write* (literal "/dev/null") (literal "/dev/zero") (regex #"^/dev/tty") (regex #"^/dev/fd/")) | internal/runner/sandbox_darwin.go:119-130 `sandboxProfile` writes ` (deny network-outbound (remote unix-socket (path-literal "` + socket + `")))` for each denied socket and ` (allow file-write* (subpath "` + root + `"))` for each root. |
| 56 | `off` MUST keep the parent environment unchanged. | With mode `off`, the runner MUST keep `DALGONA_HOME` and `DAL_HOME` in the child environment. | internal/runner/local.go:215-217 `if sandbox.Mode != SandboxOff {` / `env = withoutHomeEnvironment(env)`; internal/runner/home_leak_test.go:27-39 `TestRunnerEnvironment_keepsHomeWhenUnsandboxed` expects `DALGONA_HOME=/secret/home` in off mode; local.go:218-223 |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 20 | If a writable root does not resolve, the command MUST fail without starting. | internal/runner/sandbox_linux.go:80-81 `return nil, fmt.Errorf("resolve writable root %s: %w", path, err)`; sandbox_linux.go:43 `unix.Exec` runs only after applySandbox returns; internal/runner/sandbox_darwin.go:26-28 `r |
| 52 | On the box channel, the exec server MUST accept any requested sandbox mode when `DALGONA_SANDBOX_MODE` is unset or `off`. An unknown `DALGONA_SANDBOX_MODE` value MUST set the server start mode to `workspace-write`. | internal/runner/execserver.go:171-177 `func serverSandboxFloor` (`case SandboxOff, "":` returns off; `default:` returns workspace-write); internal/runner/wire.go:132-133 `if method == "process/start" && policy.boxChannel |

Use one word for one thing.

- Keep `thread working directory`. Replace session working directory at lines 15.
- Keep `no harness sandbox`. Replace unsandboxed process (line 48: It MUST NOT report an unsandboxed process as confined) at lines 48.

Structure:

- Line 3 joins two full sentences with a semicolon (`by accident; it is not`). ASD-STE100 forbids a semicolon that joins two sentences; replace `; it` with `. It` in the same sentence.
- The protocol workstream, Step 13 rewrites the whole section headed No harness sandbox (its Schemas S1 block). That block drops `The status row MUST show auto (no sandbox).` and `The decider state MUST carry sandboxed=false.`; agent/decider.go:159 `encoder.BoolKey("sandboxed", a.Sandboxed)` still writes the key, so keep the key in S1 if the contract needs it.
- Line 3 `outer sandbox` names the container or microVM around dalgona, not the harness sandbox. The term entry is dropped.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 24 | When the mode is `workspace-write`, the resolved directory MUST sit inside the writable roots above. The thread working directory counts as a writable root. Otherwise, the call MUST fail without starting. |
| 52 | A request that reaches the exec server over the websocket box channel MUST NOT weaken the sandbox below the server start mode. The server MUST fail such a request closed with `-32602`. |
| 56 | The daemon socket (`<home>/dalgona.sock`, mode `0600`) MUST stay out of sandboxed child environments. The runner MUST strip `DALGONA_HOME` and `DAL_HOME` from sandboxed child environments. The child cannot read the socket path from its environment. |
| 58 | Landlock filesystem rules do not mediate `connect(2)` to an existing pathname unix socket. Under this policy, a denied write outside the roots does not stop a connect to a pathname socket there. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 5 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/sandbox.md` prints `style findings: 0`, and `just verify` passes.

