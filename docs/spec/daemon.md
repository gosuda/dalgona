# Daemon service

## Run

`Run(ctx, home, newHost, serve)` MUST build the host before it touches the socket. The host factory opens the journal, and the journal takes the exclusive lock on `<home>/daemon.lock`. The daemon MUST NOT take a second lock.

When the lock is held by another process, `Run` MUST return `another dalgona process owns <home>`. It MUST NOT remove or replace `<home>/dalgona.sock` in this case.

After the lock is held, `Run` MUST remove a stale `dalgona.sock` and then listen on it. `Run` MUST create `<home>` with mode `0700` and MUST tighten an existing `<home>` to mode `0700` before it builds the host, so the host never reads config or the journal from a loose directory. It MUST set the socket file to mode `0600` after it listens, so only the owning user can reach the RPC surface. A failure to create, tighten, remove, listen, or set these permissions MUST stop `Run` with an error.

`Run` MUST call `Host.Recover` once, in the background. Recovery MUST NOT delay the accept loop. Each connection MUST get its own `serve` call, so a client gets its `initialize` answer while recovery runs. Recovery errors MUST be logged. They MUST NOT stop the daemon.

When the context ends, `Run` MUST stop accepting, close every open connection, and wait for the connection and recovery work. It MUST then close the host and the journal, which releases the lock. Closing the listener removes the socket file. An accept failure that does not come from the context MUST stop `Run` with an error.

## Ensure

`Ensure` MUST reuse an accepting socket. If the socket is unavailable, it MUST start `<executable> daemon` detached and append stdout and stderr to `daemon.log`. On Unix the child MUST run in a new session. On Windows it MUST use `DETACHED_PROCESS` and `CREATE_NEW_PROCESS_GROUP`. `Ensure` MUST retry for five seconds. On timeout it MUST return `daemon did not start: see <home>/daemon.log`.

Two clients MAY start two daemons at the same time. The lock rule above makes the second daemon exit without effect on the first.

## User service

Linux installation MUST write `~/.config/systemd/user/dalgona.service` with `ExecStart=<absolute executable> daemon` and `Restart=always`, then run `systemctl --user enable --now dalgona.service`. macOS installation MUST write `~/Library/LaunchAgents/dev.dalgona.daemon.plist` with `RunAtLoad` and `KeepAlive`, then run `launchctl bootstrap gui/<uid> <plist>`. Windows installation MUST run `schtasks /Create /SC ONLOGON /TN dalgona /TR "<absolute executable> daemon" /F`. Uninstall MUST disable the service and remove the file or task.

Service commands MUST be injectable in tests. Installation errors MUST be returned.

## Commands

`dalgona` without a command, `-p`, `--resume`, `-c`, and `models` MUST reach the core only through a daemon connection from `Ensure`. The client MUST accept `-m <model>`, `--approval auto|ask|yolo`, `--sandbox workspace-write|off`, and `-C <directory>`. With `-p`, `--json` MUST print one JSON status line. `--resume` and `-c` MUST exclude each other.

`dalgona login [provider]` and `dalgona logout <provider>` MUST use `<home>/auth.json`. Without a remote selection, these commands MUST use the client credential store and MUST NOT require a daemon connection. With `--remote <name>`, login MUST authenticate inside the box through the daemon protocol. Login without a provider MUST list providers. The daemon's credential store MUST reread `auth.json` when its size, time, or identity changes, so a new login applies to the next model request. Writers MUST serialize on `<home>/auth.lock`, MUST reread the file under that lock, and MUST change only their own provider entry. When a changed `auth.json` cannot be reread, `Mask` MUST return `secret masking failed: auth.json could not be reread` and MUST record the error. A cached OAuth token MUST be dropped when the stored credential changes outside the daemon.

`dalgona daemon` MUST run `Run` with the dalgona host. It MUST stop cleanly on interrupt and on `SIGTERM`. The host MUST start the `[mcp_servers.*]` servers once. For each thread working directory, it MUST start the servers that enabled discovery sources name for that directory, except names that `[mcp_servers.*]` already uses. A thread's skills MUST come from `<home>/skills`, `<thread cwd>/.dalgona/skills`, and discovery for that directory. MCP servers MUST start in parallel and in the background, so they do not delay the socket. Each server MUST start within 30 seconds. A server that fails or times out MUST be logged and skipped. A thread's tool list MUST wait until its servers have started or failed. MCP clients MUST be closed when the host closes. The host MUST load `<home>/plugins/*.star`, MUST log each plugin file that fails, and MUST keep the plugins that load.

The dalgona host MUST bind `[runners.<name>]` runners through the remote binder and MUST use the local runner for every other runner name. `dalgona pull <thread>` and `dalgona runner gc [--yes]` MUST run inside the daemon through the protocol extensions `dalgona/remote/pull {threadId}` and `dalgona/remote/gc {yes}`, because the daemon holds the journal lock. Each extension MUST answer `{"exitCode":<int>,"output":<text>}`. Without `--yes`, `runner gc` MUST answer no to every destroy question. `dalgona cloud up|down|pull` MUST run in the client process.

`dalgona bridge` MUST splice standard input and output to a local `Ensure` connection. `dalgona exec-server` MUST serve the exec protocol and MUST use the same splice for its `/bridge` path. The TUI, `models`, `dalgona app-server`, `dalgona web`, `pull`, and `runner gc` MUST dial the local daemon through `Ensure`. When `--remote <name>` is given, or `remote` is set in configuration, they MUST dial the remote box daemon through the binder's bridge instead. The flag MUST win over the configuration. `bridge` and `exec-server` MUST NOT redirect their local listener with `--remote`.

`--config <key>=<value>`, `--config=<key>=<value>`, and `-c <key>=<value>` pairs MAY come before the `app-server` subcommand, as the Codex clients send them. When the first argument after these pairs is `app-server`, the pairs MUST reach the app-server command unchanged. In every other case, `-c` keeps its client meaning.

`dalgona service install|uninstall` MUST install or remove the platform user service. `dalgona codex install|uninstall|status` MUST manage or inspect the desktop override. `dalgona codex record --real <path> [args...]` MUST run the selected Codex executable and record protocol metadata in `<home>/census/<unix-seconds>.jsonl`. Without forwarded arguments, it MUST run `app-server`.

`dalgona run <file.star> [--arg k=v ...]` MUST run a durable Starlark workflow as defined in [starlark.md](starlark.md).

Inside herdr (`HERDR_ENV=1` and `HERDR_PANE_ID` set), the TUI status MUST be reported as `working` while a turn runs, `blocked` while an approval waits, and `idle` otherwise. The reported `--seq` MUST be the sequence the TUI reports for the thread. A status without a thread MUST NOT be reported. While the thread stays the same, a status equal to the previous one or with a lower sequence MUST NOT be reported. A change of thread MUST reset this comparison. When `[herdr] subagent_panes` is true, each child spawn MUST open a pane that resumes the child. herdr commands MUST run on one worker, so they never block the TUI. Each command MUST time out after five seconds. While the worker is busy, only the newest status for each thread MUST wait; at most 64 pane requests MAY wait. On exit, the process MUST wait at most five seconds for waiting commands, MUST then cancel any command still running, and MUST then release the pane.
