# Daemon service

## Run

`Run(ctx, home, newHost, serve)` MUST build the host before it touches the socket. The host factory opens the journal, and the journal takes the exclusive lock on `<home>/daemon.lock`. The daemon MUST NOT take a second lock.

When the lock is held by another process, `Run` MUST return `another dalgona process owns <home>`. It MUST NOT remove or replace `<home>/dalgona.sock` in this case.

`Run` MUST create `<home>` with mode `0700`. `Run` MUST tighten an existing `<home>` to mode `0700` before it builds the host. The host therefore never reads config or the journal from a loose directory. On Windows the create step MUST set a protected DACL that grants access only to the current user, SYSTEM, and Administrators. The tighten step MUST reapply that DACL to the home and to every file and directory inside it. The tighten step MUST NOT change a symlink or an irregular entry. It MUST set the socket file to mode `0600` after it listens, so only the owning user can reach the RPC surface. A failure to create, tighten, remove, listen, or set these permissions MUST stop `Run` with an error.

`Run` MUST accept a connection only when the peer uid equals its own uid. On Linux it MUST read the peer uid with `SO_PEERCRED`. On macOS and FreeBSD it MUST read the peer uid with `LOCAL_PEERCRED`. On other unix targets the peer check MUST fail closed and refuse the connection. On Windows it MUST enforce the protected home DACL and MUST NOT apply a peer uid check.

On Linux `Run` MUST also refuse a peer with `no_new_privs` set, unless the daemon itself has `no_new_privs` set. It MUST read the peer mark from `/proc/<pid>/status` for the pid from `SO_PEERCRED`. When the kernel supports `SO_PEERPIDFD`, it MUST hold the peer with the pidfd and confirm after the read that the same process is still alive. If the kernel reports `ENOPROTOOPT`, it MUST use the pid alone. Any other pidfd error MUST refuse the peer. It MUST fail closed when the peer status is unreadable. `Ensure` MUST check only the peer uid when it dials. A client MUST NOT refuse a daemon for this mark. When the daemon itself has `no_new_privs` set, `Run` MUST log one warning at start that sandboxed commands can reach it, and it MUST still start. When a client with `no_new_privs` set fails its first exchange, the client MUST report that the daemon refuses clients that run inside a sandbox.

The peer check MUST NOT count as a boundary against the same uid: any unsandboxed same-uid process that reaches `<home>/dalgona.sock` can speak the RPC protocol. The runner keeps the home path out of sandboxed child environments (see [sandbox.md](sandbox.md)), but the default path stays guessable, so this hiding is best-effort only.

Administrators and SYSTEM are inside the boundary. The check covers the home directory entry. On Windows, the daemon resets the entries inside the home at start. On other systems, it changes only the home directory mode. An entry loosened afterwards on Windows stays loosened until the next daemon start.

`Run` MUST call `Host.Recover` once, in the background. Recovery MUST NOT delay the accept loop. Each connection MUST get its own `serve` call, so a client gets its `initialize` answer while recovery runs. `Run` MUST log recovery errors. Recovery errors MUST NOT stop the daemon.

When the context ends, `Run` MUST stop accepting, close every open connection, and wait for the connection and recovery work. It MUST then close the host and the journal, which releases the lock. Closing the listener removes the socket file. An accept failure that does not come from the context MUST stop `Run` with an error.

`Ensure` MUST tighten `<home>` to mode `0700`. `Ensure` MUST refuse a home that is not owned by the current uid. `Ensure` MUST refuse a home whose mode is not `0700`. `Ensure` MUST refuse a socket that is not owned by the current uid. `Ensure` MUST verify the peer uid of each dialed connection and MUST close a connection whose peer uid differs, without sending any request on it. A pre-bound socket from another user MUST be refused, never answered. On Windows `Ensure` MUST protect the home before it verifies the home. It MUST refuse a home that still grants access to any other principal. It MUST NOT apply uid checks. On other unix targets without a peer credential call, the peer check MUST fail closed and `Ensure` MUST refuse the socket.

`Ensure` MUST reuse an accepting socket. If the socket is missing or stale, it MUST start `<executable> daemon` detached and append stdout and stderr to `daemon.log`. On Unix the child MUST run in a new session. On Windows it MUST use `DETACHED_PROCESS` and `CREATE_NEW_PROCESS_GROUP`. `Ensure` MUST retry for five seconds. On timeout it MUST return `daemon did not start: see <home>/daemon.log`.

`Ensure` and `Run` MUST reject a socket path of `<limit>` bytes or more before spawning or listening, with `daemon socket path <path> is <n> bytes; the limit is <limit>; use a shorter home directory`.

The limit MUST be 104 on darwin and the BSDs and 108 on Linux and Windows.

Two clients MAY start two daemons at the same time. The lock rule above makes the second daemon exit without effect on the first.

## User service

Installation MUST quote the executable path in the systemd unit (`ExecStart="<absolute executable>" daemon`) and MUST XML-escape it in the launchd plist. When `DALGONA_HOME` is set at install time, the unit and plist MUST pin `DALGONA_HOME` to that value. The login-time daemon therefore uses the installing client's home. Linux installation MUST write `~/.config/systemd/user/dalgona.service` with `ExecStart="<absolute executable>" daemon` and `Restart=always`, then run `systemctl --user enable --now dalgona.service`. macOS installation MUST write `~/Library/LaunchAgents/dev.dalgona.daemon.plist` with `RunAtLoad` and `KeepAlive`, then run `launchctl bootstrap gui/<uid> <plist>`. Windows installation MUST run `schtasks /Create /SC ONLOGON /TN dalgona /TR "<absolute executable> daemon" /F`. Uninstall MUST disable the service and remove the service definition. It MUST remove the service definition even when disabling fails, MUST tolerate a service that is not loaded, and MUST report any remaining errors.

Service commands MUST be injectable in tests. Installation errors MUST be returned.

## Commands

`dalgona` without a command, `-p`, `--resume`, `-c`, and `models` MUST reach the core only through a daemon connection from `Ensure`. The client MUST accept `-m <model>`, `--approval auto|ask|yolo`, `--sandbox workspace-write|off`, and `-C <directory>`. With `-p`, `--json` MUST print one JSON status line. `--resume` and `-c` MUST exclude each other.

The status line MUST be one JSON object with the keys `thread`, `status`, and `stop_reason`.

`dalgona login [provider]` and `dalgona logout <provider>` MUST use `<home>/auth.json`. Without a remote selection, these commands MUST use the client credential store and MUST NOT require a daemon connection. With `--remote <name>`, login for an API-key login target MUST store the key inside the box through the daemon protocol. With `--remote <name>`, login for `chatgpt` MUST use the device-code flow inside the box. With `--remote <name>`, login for `antigravity`, `cursor`, or `devin` MUST fail before it dials the box, with this text:

```text
login <provider> cannot run inside remote <name>; run `dalgona login <provider>` here, then `dalgona cloud up <name> --copy-auth`
```

Login without a provider MUST list providers. The daemon's credential store MUST reread `auth.json` when its size, time, or identity changes, so a new login applies to the next model request. Writers MUST serialize on `<home>/auth.lock`, MUST reread the file under that lock, and MUST change only their own provider entry. When a changed `auth.json` cannot be reread, `Mask` MUST return `secret masking failed: auth.json could not be reread` and MUST record the error. The daemon MUST drop a cached OAuth token when the stored credential changes outside the daemon.

A conditional credential writer MUST reload `auth.json` under `<home>/auth.lock`, MUST write only on a match, and MUST report whether it wrote.

The API-key login targets are `anthropic`, `google`, `openai`, `openrouter`, `cloudflare`, `sandbox0`, and `sprites`. With `--api-key <key>`, login MUST store the key without a prompt. The `--api-key` option works only for the API-key login targets. For another provider, login fails with `login <provider>: --api-key works only for <targets>`.

`login chatgpt --device-auth` MUST sign in with the ChatGPT device code and print `Open <url> and enter the code <code> to log in to chatgpt.`. With any other provider, the flag MUST fail with `login <provider>: --device-auth works only for chatgpt`.

With `--remote <name>`, `logout` MUST remove the provider credential inside the box through the daemon protocol. With `--remote <name>`, login without a provider MUST show the account of the box.

`dalgona daemon` MUST run `Run` with the dalgona host. It MUST stop cleanly on interrupt and on `SIGTERM`. The host MUST start the `[mcp_servers.*]` servers once. For each thread working directory, it MUST start the servers that enabled discovery sources name for that directory, except names that `[mcp_servers.*]` already uses. A thread's skills MUST come from `<home>/skills`, `<thread cwd>/.dalgona/skills`, and discovery for that directory. MCP servers MUST start in parallel and in the background, so they do not delay the socket. Each server MUST start within 30 seconds. A server that fails or times out MUST be logged and skipped. A thread's tool list MUST wait until its servers have started or failed. The host MUST close its MCP clients when it closes. The host MUST load `<home>/plugins/*.star`. It MUST log one error that names each plugin file that fails. It MUST keep the plugins that load.

A top-level dalgona thread MUST get the `ratchet` tool. Child and advisor threads MUST NOT get it.

The dalgona host MUST bind `[runners.<name>]` runners through the remote binder and MUST use the local runner for every other runner name. `dalgona pull <thread>` and `dalgona runner gc [--yes]` MUST run inside the daemon through the protocol extensions `dalgona/remote/pull {threadId}` and `dalgona/remote/gc {yes}`, because the daemon holds the journal lock. Each extension MUST answer `{"exitCode":<int>,"output":<text>}`. Without `--yes`, `runner gc` MUST answer no to every destroy question. `dalgona cloud up|down|pull` MUST run in the client process.

`dalgona bridge` MUST splice standard input and output to a local `Ensure` connection. `dalgona exec-server` MUST serve the exec protocol and MUST use the same splice for its `/bridge` path. The TUI, `models`, `dalgona app-server`, `dalgona web`, `pull`, and `runner gc` MUST dial the local daemon through `Ensure`. When `--remote <name>` is given, or `remote` is set in configuration, they MUST dial the remote box daemon through the binder's bridge instead. The flag MUST win over the configuration. `bridge` and `exec-server` MUST NOT redirect their local listener with `--remote`.

`--config <key>=<value>`, `--config=<key>=<value>`, and `-c <key>=<value>` pairs MAY come before the `app-server` subcommand, as the Codex clients send them. When the first argument after these pairs is `app-server`, the pairs MUST reach the app-server command unchanged. In every other case, `-c` keeps its client meaning.

`dalgona service install|uninstall` MUST install or remove the platform user service. `dalgona codex install|uninstall|status` MUST manage or inspect the desktop override. `dalgona codex record --real <path> [args...]` MUST run the selected Codex executable and record protocol metadata in `<home>/census/<unix-seconds>.jsonl`. Without forwarded arguments, it MUST run `app-server`.

`dalgona run <file.star> [--arg k=v ...]` MUST run a durable Starlark workflow as defined in [starlark.md](starlark.md).

Inside herdr (`HERDR_ENV=1` and `HERDR_PANE_ID` set), the TUI status MUST be reported. While a turn runs, the reported status MUST be `working`. While an approval waits, the reported status MUST be `blocked`. Otherwise, the reported status MUST be `idle`. The reported `--seq` MUST be the sequence the TUI reports for the thread. A status without a thread MUST NOT be reported. While the thread stays the same, a status equal to the previous one or with a lower sequence MUST NOT be reported. A change of thread MUST reset this comparison. When `[herdr] subagent_panes` is true, each subagent spawn MUST open a pane that resumes the subagent. The herdr commands MUST run on one worker, so they never block the TUI. Each command MUST time out after five seconds. While the worker is busy, only the newest status for each thread MUST wait. The worker holds at most 64 pane requests. It drops any later pane request. On exit, the process MUST wait at most five seconds for waiting commands. After that wait, the process MUST cancel any command that still runs. Then the process MUST release the pane.
