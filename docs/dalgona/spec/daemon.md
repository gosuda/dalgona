# Daemon service

## Run

`Run(ctx, home, newHost, serve)` MUST build the host before it touches the socket. The host factory opens the journal, and the journal takes the exclusive lock on `<home>/daemon.lock`. The daemon MUST NOT take a second lock.

When the lock is held by another process, `Run` MUST return `another dalgona process owns <home>`. It MUST NOT remove or replace `<home>/dalgona.sock` in this case.

`Run` MUST create `<home>` with mode `0700`. `Run` MUST tighten an existing `<home>` to mode `0700` before it builds the host. The host therefore never reads config or the journal from a loose directory. On Windows the create step MUST set a protected DACL that grants access only to the current user, SYSTEM, and Administrators. The tighten step MUST reapply that DACL to the home and to every file and directory inside it. The tighten step MUST NOT change a symlink or an irregular entry. It MUST set the socket file to mode `0600` after it listens, so only the owning user can reach the RPC surface. A failure to create, tighten, remove, listen, or set these permissions MUST stop `Run` with an error.

`Run` MUST accept a connection only when the peer uid equals its own uid. On Linux it MUST read the peer uid with `SO_PEERCRED`. On macOS and FreeBSD it MUST read the peer uid with `LOCAL_PEERCRED`. On other unix targets the peer check MUST fail closed and refuse the connection. On Windows it MUST enforce the protected home DACL and MUST NOT apply a peer uid check.

On Linux `Run` MUST also refuse a peer with `no_new_privs` set, unless the daemon itself has `no_new_privs` set. It MUST read the peer mark from `/proc/<pid>/status` for the pid from `SO_PEERCRED`. When the kernel supports `SO_PEERPIDFD`, it MUST hold the peer with the pidfd and confirm after the read that the same process is still alive. If the kernel reports `ENOPROTOOPT`, it MUST use the pid alone. Any other pidfd error MUST refuse the peer. It MUST fail closed when the peer status is unreadable. `Ensure` MUST check only the peer uid when it dials. A client MUST NOT refuse a daemon for this mark. When the daemon itself has `no_new_privs` set, `Run` MUST log one warning at start that sandboxed commands can reach it, and it MUST still start. Before response data arrives, a client with `no_new_privs` MUST report a connection failure as a sandbox refusal. A clean EOF after response data MUST remain EOF and MUST NOT report a sandbox refusal.

The peer check MUST NOT count as a boundary against the same uid: any unsandboxed same-uid process that reaches `<home>/dalgona.sock` can speak the RPC protocol. The runner keeps the home path out of sandboxed child environments (see [sandbox.md](../../dal/spec/sandbox.md)), but the default path stays guessable, so this hiding is best-effort only.

Administrators and SYSTEM are inside the boundary. The check covers the home directory entry. On Windows, the daemon resets the entries inside the home at start. On other systems, it changes only the home directory mode. An entry loosened afterwards on Windows stays loosened until the next daemon start.

`Run` MUST call `Host.Recover` once, in the background. Recovery MUST NOT delay the accept loop. Each connection MUST get its own `serve` call, so a client gets its `initialize` answer while recovery runs. `Run` MUST log recovery errors. Recovery errors MUST NOT stop the daemon.

When the context ends, `Run` MUST stop accepting, close every open connection, and wait for the connection and recovery work. It MUST then close the host and the journal, which releases the lock. Closing the listener removes the socket file. An accept failure that does not come from the context MUST stop `Run` with an error.

`Ensure` MUST tighten `<home>` to mode `0700`. `Ensure` MUST refuse a home that is not owned by the current uid. `Ensure` MUST refuse a home whose mode is not `0700`. `Ensure` MUST refuse a socket that is not owned by the current uid. `Ensure` MUST verify the peer uid of each dialed connection and MUST close a connection whose peer uid differs, without sending any request on it. A pre-bound socket from another user MUST be refused, never answered. On Windows `Ensure` MUST protect the home before it verifies the home. It MUST refuse a home that still grants access to any other principal. It MUST NOT apply uid checks. On other unix targets without a peer credential call, the peer check MUST fail closed and `Ensure` MUST refuse the socket.

`Ensure` MUST reuse an accepting socket. If the socket is missing or stale, it MUST start `<executable> daemon` detached and append stdout and stderr to `daemon.log`. On Unix the child MUST run in a new session. On Windows it MUST use `DETACHED_PROCESS` and `CREATE_NEW_PROCESS_GROUP`. `Ensure` MUST retry for five seconds. On timeout it MUST return `daemon did not start: see <home>/daemon.log`.

Local clients connect through a `client.Session` built from a backend: the absolute daemon executable without symlinks and its build. A build MUST be the first 16 hex digits of the SHA-256 of the executable bytes. A client that cannot read its backend MUST fail before it connects. `client.Build()` MUST return the build of the running executable, which the daemon reports as its identity. A read failure there MUST return the empty string. An empty daemon identity MUST NOT trigger a restart.

Until the session's first successful connection, `Connect` MUST run `Ensure` with the backend executable and check the daemon build identity before any other request. After that success, `Connect` MUST only dial the trusted socket, and it MUST NOT start a daemon. `Reconnect` MUST only dial the trusted socket. `Close` MUST close every connection the session returned and MUST NOT retire the daemon. A daemon that reports a build other than the backend build MUST produce a mismatch.

A terminal client MUST ask with this prompt:

```text
daemon <home> runs build <running>; this client is build <own>. Restart the daemon? Running turns stop and recover. [y/N]
```

The answer `y` MUST restart through the session with the identity the mismatch reported. The restart MUST probe the daemon again and retire it only when its pid, build, and start time still match. It MUST then wait up to five seconds for the socket to go and run `Ensure` with the backend. A daemon that changed since the mismatch MUST stay running, and the client MUST answer:

```text
daemon changed before restart: saw pid <pid> build <build>, now pid <pid> build <build>; reconnect and retry
```

Any other answer MUST give this error:

```text
daemon build mismatch: restart the daemon with dalgona restart
```

A client without a terminal MUST give this error and exit 1:

```text
daemon runs build <running>, this client is build <own>; run dalgona restart
```

A daemon without the identity method MUST count as build `<unknown pre-identity build>`, not as compatible.

`Ensure` and `Run` MUST reject a socket path of `<limit>` bytes or more before spawning or listening, with `daemon socket path <path> is <n> bytes; the limit is <limit>; use a shorter home directory`.

The limit MUST be 104 on darwin and the BSDs and 108 on Linux and Windows.

Two clients MAY start two daemons at the same time. The lock rule above makes the second daemon exit without effect on the first.


## After-recovery callbacks

`Run` accepts variadic after-recovery callbacks of this type:

```go
RecoveredFunc func(context.Context, *agent.Host, error)
```

The daemon MUST run `Host.Recover` in the background without delaying the accept loop. After recovery finishes, `Run` MUST invoke each callback in registration order with the context, the host, and the recovery result. A nil error means recovery succeeded. A non-nil error carries the recovery failure.

Callbacks MUST run on the same background task as recovery. They MUST NOT delay the accept loop. Recovery errors MUST be logged and MUST NOT stop the daemon. Callback failures MUST NOT stop the daemon either. Each connection still gets its own `serve` call while recovery and callbacks run.

Daltoki starts through this seam. Its battery callback runs post-`Recover` and starts the coordinator exactly once. A recovery failure MUST disable Daltoki dispatch with `daltoki: host recovery failed`. The disabled coordinator MUST still answer an empty status.

`Ensure`, shutdown, and retire behavior is unchanged. When the context ends, `Run` MUST still stop accepting, wait for the connection and recovery work including callbacks, then close the host and the journal.
## User service

Installation MUST quote the executable path in the systemd unit (`ExecStart="<absolute executable>" daemon`) and MUST XML-escape it in the launchd plist. When `DALGONA_HOME` is set at install time, the unit and plist MUST pin `DALGONA_HOME` to that value. The login-time daemon therefore uses the installing client's home. Linux installation MUST write `~/.config/systemd/user/dalgona.service` with `ExecStart="<absolute executable>" daemon` and `Restart=always`, then run `systemctl --user enable --now dalgona.service`. macOS installation MUST write `~/Library/LaunchAgents/dev.dalgona.daemon.plist` with `RunAtLoad` and `KeepAlive`, then run `launchctl bootstrap gui/<uid> <plist>`. Windows installation MUST run `schtasks /Create /SC ONLOGON /TN dalgona /TR "<absolute executable> daemon" /F`. Uninstall MUST disable the service and remove the service definition. It MUST remove the service definition even when disabling fails, MUST tolerate a service that is not loaded, and MUST report any remaining errors.

Service commands MUST be injectable in tests. Installation errors MUST be returned.

## Commands

`dalgona` without a command, `-p`, `--resume`, `-c`, and `models` MUST reach the core only through a daemon connection from `Ensure`.
The client MUST accept `-m <model>`, `--approval auto|ask|yolo`, `--sandbox workspace-write|off`, and `-C <directory>`.
`--resume` and `-c` MUST exclude each other.

With `-p --json`, stdout MUST contain one JSONL line for each completed item of the selected thread.
The client MUST copy the masked `item` value without removing extension members.
Each item line MUST contain `schemaVersion:1`, `type:"item"`, `thread`, `turn`, and `item`.
The client MUST write item lines immediately, in notification order.
The final result line MUST contain `schemaVersion:1`, `type:"result"`, `thread`, `status`, and `stop_reason`.
Stderr MUST contain diagnostics only.
A write failure MUST stop output and further prompt submission.

Headless runs MUST read non-terminal stdin before any daemon request.
The client MUST wait for EOF without a deadline.
Use `</dev/null` or `ssh -n` when stdin is not prompt input.
Input above 1048576 bytes MUST fail before any request.
The client MUST replace invalid UTF-8 with U+FFFD.
The client MUST remove trailing newlines from stdin.
With both inputs present, the client MUST join stdin and the prompt with two newlines.
With an empty prompt, stdin alone MUST supply the prompt.
Empty stdin MUST leave the prompt unchanged.
Without either input, `-p` MUST require `--resume` or `-c`.

`dalgona login [provider]` and `dalgona logout <provider>` MUST use `<home>/auth.json`. Without a remote selection, these commands MUST use the client credential store and MUST NOT require a daemon connection. With `--remote <name>`, login for an API-key login target MUST store the key inside the box through the daemon protocol. With `--remote <name>`, login for `chatgpt` MUST use the device-code flow inside the box. With `--remote <name>`, login for `antigravity`, `cursor`, or `devin` MUST fail before it dials the box, with this text:

```text
login <provider> cannot run inside remote <name>; run `dalgona login <provider>` here, then `dalgona cloud up <name> --copy-auth`
```

Login without a provider MUST list providers. The daemon's credential store MUST reread `auth.json` when its size, time, or identity changes, so a new login applies to the next model request. Writers MUST serialize on `<home>/auth.lock`, MUST reread the file under that lock, and MUST change only their own provider entry. When a changed `auth.json` cannot be reread, `Mask` MUST return `secret masking failed: auth.json could not be reread` and MUST record the error. The daemon MUST drop a cached OAuth token when the stored credential changes outside the daemon.

A conditional credential writer MUST reload `auth.json` under `<home>/auth.lock`, MUST write only on a match, and MUST report whether it wrote.

The API-key login targets are `anthropic`, `google`, `openai`, `openrouter`, `cloudflare`, `sandbox0`, and `sprites`. With `--api-key <key>`, login MUST store the key without a prompt. The `--api-key` option works only for the API-key login targets. For another provider, login fails with `login <provider>: --api-key works only for <targets>`.

Without `--api-key` and without `--remote`, `login anthropic` MUST start the OAuth code flow with PKCE in the client.

`login chatgpt --device-auth` MUST sign in with the ChatGPT device code and print `Open <url> and enter the code <code> to log in to chatgpt.`. With any other provider, the flag MUST fail with `login <provider>: --device-auth works only for chatgpt`.

With `--remote <name>`, `logout` MUST remove the provider credential inside the box through the daemon protocol. With `--remote <name>`, login without a provider MUST show the account of the box.

`dalgona daemon` MUST run `Run` with the dalgona host. It MUST stop cleanly on interrupt and on `SIGTERM`. The host MUST start the `[mcp_servers.*]` servers once. For each thread working directory, it MUST start the servers that enabled discovery sources name for that directory, except names that `[mcp_servers.*]` already uses. A thread's skills MUST come from `<home>/skills`, `<thread cwd>/.dalgona/skills`, and discovery for that directory. MCP servers MUST start in parallel and in the background, so they do not delay the socket. Each server MUST start within 30 seconds. A server that fails or times out MUST be logged and skipped. A thread's tool list MUST wait until its servers have started or failed. The host MUST close its MCP clients when it closes. The host MUST load `<home>/plugins/*.star`. It MUST log one error that names each plugin file that fails. It MUST keep the plugins that load.

A top-level dalgona thread MUST get the `ratchet` tool. Child and advisor threads MUST NOT get it.

`dalgona restart` MUST probe the daemon, then restart it through a session with that identity, so it retires only the daemon it identified. It MUST print `daemon restarted (build <own>)`. With no daemon running it MUST print `daemon started (build <own>)`. It MUST terminate only a daemon whose recorded pid, current-user ownership, and home association are verified. A daemon without the identity method MUST stay running, and the command MUST answer:

```text
cannot safely identify the old daemon; stop it manually and retry dalgona restart
```

The dalgona host MUST bind `[runners.<name>]` runners through the remote binder and MUST use the local runner for every other runner name. `dalgona pull <thread>` and `dalgona runner gc [--yes]` MUST run inside the daemon through the protocol extensions `dalgona/remote/pull {threadId}` and `dalgona/remote/gc {yes}`, because the daemon holds the journal lock. Each extension MUST answer `{"exitCode":<int>,"output":<text>}`. Without `--yes`, `runner gc` MUST answer no to every destroy question. `dalgona cloud up|down|pull` MUST run in the client process.

`dalgona bridge` MUST splice standard input and output to a local session connection. `dalgona exec-server` MUST serve the exec protocol and MUST use the same splice for its `/bridge` path. The TUI, `models`, `dalgona app-server`, `dalgona web`, `dalgona acp`, `pull`, and `runner gc` MUST open one session per process and dial the local daemon through its `Connect`. When `--remote <name>` is given, or `remote` is set in configuration, the session MUST dial the remote box daemon through the `client.Remotes` bridge instead. The flag MUST win over the configuration. `bridge` and `exec-server` MUST NOT redirect their local listener with `--remote`.

`--config <key>=<value>`, `--config=<key>=<value>`, and `-c <key>=<value>` pairs MAY come before the `app-server` subcommand, as the Codex clients send them. When the first argument after these pairs is `app-server`, the pairs MUST reach the app-server command unchanged. In every other case, `-c` keeps its client meaning.

The `--config` argument MUST split at its first `=`. A `--config` argument without `=`, or with an empty key, MUST fail with exit status 2. A `--config` value MUST lose its surrounding whitespace and quote characters. A `--config` key or value that dalgona does not map MUST be ignored with a note. A later mapped `--config` value for a key MUST replace an earlier one. The `--config` split MUST follow the Codex rule in `codex-rs/utils/cli/src/config_override.rs`.

The app-server `--listen` value MUST be `stdio://` or `ws://IP:PORT`. The WebSocket address MUST be a literal IP address with a port. Any other `--listen` value MUST fail with exit status 2 before the listener starts. A non-loopback address MUST fail even when WebSocket authentication is configured. The loopback rule MUST follow Codex `codex-rs/app-server-transport/src/transport/websocket.rs`.

On stdin EOF, the stdio app-server MUST close the daemon connection's write side.
It MUST continue to forward daemon output until the daemon closes that connection.
The daemon MUST drain admitted requests and flush replies when handlers finish within its 30-second drain limit.
The app-server MUST then exit without stopping the shared daemon.
The local daemon dialer MUST preserve the Unix socket's half-close capability.

The app-server WebSocket listener MUST refuse a request with a non-empty `Origin` header with HTTP 403. It MUST answer a request without a valid `Authorization: Bearer <token>` header with HTTP 401. It MUST compare the bearer token digest in constant time. It MUST answer HTTP 503 while it shuts down, and HTTP 503 when it cannot dial the daemon. It MUST NOT upgrade a request that the daemon dial refused. The Origin refusal MUST follow Codex `codex-rs/app-server-transport/src/transport/websocket.rs`.

The WebSocket server MUST set a read timeout of 30 seconds and a write timeout of 60 seconds. It MUST set an idle timeout of 120 seconds, in addition to its 10-second header read timeout.

`dalgona acp` MUST serve the Agent Client Protocol on standard input and output and MUST take no arguments. It MUST dial the daemon through the same `Ensure` path as the other local clients, or through the remote box when `remote` is configured. Each ACP session MUST map to one daemon thread.

The adapter MUST read one JSON-RPC message per line of at most 16 MiB. A line that is not JSON MUST get a `-32700` reply with id `null`. A JSON value that is not a request MUST get `-32600`. An error reply without a known request id MUST carry id `null`. A message without `id` is a notification and MUST NOT get a reply. An unknown method MUST get `-32601` with `Method not found`. At most 64 requests MAY be in flight. The adapter MUST answer further requests with `-32603` and `adapter busy: too many open requests`.

`initialize` MUST require `protocolVersion` and MUST fail with `-32602` and `missing protocolVersion` without it. On the first `initialize` the adapter MUST dial the daemon and run `initialize` as client `dalgona-acp` with `requiredHumanApproval`. It MUST answer only after that exchange. The result MUST carry protocol version 1, `loadSession: true`, image prompt support, `sessionCapabilities.list`, and empty `authMethods`. MCP `http` and `sse` capabilities MUST be false. Later calls MUST reuse the connection. Session methods before `initialize` MUST fail with `-32600` and `initialize has not run`. A closed daemon connection MUST make an in-flight prompt fail with `the daemon connection closed`.

`session/new` MUST take `cwd` and MUST fail with `-32602` and `missing cwd` without it. It MUST call `thread/start` and return the thread id as `sessionId`. `mcpServers` entries MUST be ignored. After the reply the adapter MUST send one `agent_message_chunk` that states dalgona ignores them. `session/list` MUST call `thread/list` with the cursor and filter by `cwd` when given. Each entry MUST contain `sessionId`, `cwd`, `title` from the thread name or preview, and `updatedAt` in RFC 3339. `session/load` MUST take `sessionId` and fail `-32602` `missing sessionId` without it. It MUST call `thread/resume` with `excludeTurns`, then MUST page `thread/turns/list` ascending with `itemsView: full` and 100 turns per page. It MUST emit `user_message_chunk` or `agent_message_chunk` for each replayed item text.

`session/prompt` MUST reject an unknown session with `-32602` and a second prompt on a busy session with `-32600`. It MUST accept only `text` and `image` blocks and MUST fail other blocks with `-32602` and `unsupported prompt block`. It MUST call `turn/start` and MUST block until `turn/completed`. `stopReason` MUST be `end_turn` on a completed turn and `cancelled` on an interrupted one. A failed turn MUST return `-32603` with the turn error message. Any other turn status MUST return `-32603`. While a prompt runs the adapter MUST forward daemon item and turn notifications as `session/update`.

`session/cancel` MUST NOT fail for an unknown session. For a running turn it MUST send `turn/interrupt`. A cancel that arrives before the turn id is known MUST interrupt once `turn/start` returns the id.

For `item/commandExecution/requestApproval` and `item/fileChange/requestApproval` the adapter MUST send `session/request_permission` with the options `allow_once` (Allow once), `allow_always` (Allow for this thread), and `reject_once` (Reject). The tool call MUST be `execute` for a command, `edit` for a file change, and `switch_mode` when a plan path is present. `allow_always` MUST appear only when the daemon did not require human approval, gave no plan path, and listed `acceptForSession` in `availableDecisions`. A cancelled outcome MUST answer `cancel`. A selected option MUST answer its mapped decision of `accept`, `acceptForSession`, or `decline`. Any other outcome MUST answer `decline`. `item/tool/requestUserInput` MUST fail with `-32601` and `ask is not supported by this client`. `session/set_config_option` with `configId` `model` MUST fail with `-32601` and `session model is not supported; set model in config.toml`. Every other config id MUST fail with `-32601` and `Method not found`.

`dalgona service install|uninstall` MUST install or remove the platform user service. `dalgona codex install|uninstall|status` MUST manage or inspect the desktop override. `dalgona codex record --real <path> [args...]` MUST run the selected Codex executable and record protocol metadata in `<home>/census/<unix-seconds>.jsonl`. Without forwarded arguments, it MUST run `app-server`.

`dalgona run <file.star> [--arg k=v ...]` MUST run a durable Starlark workflow as defined in [starlark.md](../../dal/spec/starlark.md).

Inside herdr (`HERDR_ENV=1` and `HERDR_PANE_ID` set), the TUI status MUST be reported. While a turn runs, the reported status MUST be `working`. While an approval waits, the reported status MUST be `blocked`. Otherwise, the reported status MUST be `idle`. The reported `--seq` MUST be the sequence the TUI reports for the thread. A status without a thread MUST NOT be reported. While the thread stays the same, a status equal to the previous one or with a lower sequence MUST NOT be reported. A change of thread MUST reset this comparison. When `[herdr] subagent_panes` is true, each subagent spawn MUST open a pane that resumes the subagent. The herdr commands MUST run on one worker, so they never block the TUI. Each command MUST time out after five seconds. While the worker is busy, only the newest status for each thread MUST wait. The worker holds at most 64 pane requests. It drops any later pane request. On exit, the process MUST wait at most five seconds for waiting commands. After that wait, the process MUST cancel any command that still runs. Then the process MUST release the pane.
