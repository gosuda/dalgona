# Doctor

## Command

`dalgona doctor [--online]` MUST print one row for each check result. Each row MUST read `<status> <check>: <detail>`. The status MUST be `ok`, `warn` or `fail`.

Any other argument MUST print `usage: dalgona doctor [--online]` on standard error and MUST exit 2.

The command MUST exit 1 when at least one row has the status `fail`. Otherwise the command MUST exit 0. A `warn` row MUST NOT change the exit code.

The command MUST read the home, the config, the journal directories, the daemon socket, the daemon log, the plugin files and the stored credentials. The command MUST NOT write any file.

The command MUST NOT start a daemon. The command MUST NOT run a command in the sandbox. The command MUST NOT start an MCP server. The command MUST NOT refresh a token.

Every detail MUST pass through the secret masker of the stored credentials. Every control character in a detail MUST become a space. A detail longer than 400 characters MUST end with `...`.

## Modes

Offline mode is the default. Offline mode MUST send no request to a provider.

`--online` adds one model-list request for each provider that has a credential. Each request MUST end after 5 seconds. A request MUST NOT retry. A request MUST NOT rotate accounts or fall back to another provider. A request MUST NOT start inference.

A model-list refusal, timeout or error MUST give `fail provider-models: <id>: <masked cause>`. A model list that arrives MUST give `ok provider-models: <id>: <n> models`.

A provider that has a credential but no model list to read MUST give `warn provider-models: <id>: not configured; no request made`.

## Rows

The command MUST run the checks in this order: `home`, `config`, `daemon` with `build`, `threads`, `providers`, `provider-models`, `sandbox`, `tls`, `mcp` and `plugins`. The `provider-models` check runs only with `--online`.

| Check | Row | Meaning |
| --- | --- | --- |
| `home` | `ok home: <home>` | The home belongs to the user and has mode `0700`. |
| `home` | `fail home: <text>` | The text is the error of `daemon.VerifyHome`. |
| `config` | `ok config: valid` | `config.Load` accepts the config files. |
| `config` | `fail config: <error>` | The error names the file and the line. |

The `daemon` check MUST dial the socket with the owner checks of `client.Ensure`. It MUST NOT start a daemon. It MUST send one `dalgona/daemon/identity` request when the dial succeeds.

| Check | Row | Meaning |
| --- | --- | --- |
| `daemon` | `ok daemon: not running; it starts on demand` | No socket exists. |
| `daemon` | `warn daemon: stale socket <path>; any dalgona command replaces it` | The socket refuses a dial. |
| `daemon` | `ok daemon: running pid <n> up <duration>` | The daemon answers the identity request. |
| `daemon` | `ok daemon: running; it reports no identity` | The daemon predates the identity request. |
| `daemon` | `fail daemon: the socket belongs to another user: <error>` | The owner check refuses the socket. |
| `daemon` | `fail daemon: cannot inspect the running daemon: <error>` | The identity request fails. |
| `daemon` | `fail daemon: cannot reach the socket: <error>` | No socket file exists and the dial still fails, for example a home path that is too long. |

The `build` check MUST follow the `daemon` row in the same run.

| Check | Row | Meaning |
| --- | --- | --- |
| `build` | `ok build: <id>` | The daemon build equals this binary, or no daemon runs. |
| `build` | `fail build: daemon runs build <a>, this binary is build <b>; run dalgona restart` | The builds differ. |
| `build` | `warn build: not checked; the daemon cannot be inspected` | The identity request failed. |
| `build` | `warn build: not checked; the socket is not trusted` | The owner check refused the socket. |
| `build` | `warn build: not checked; the socket cannot be reached` | The dial failed without a socket file. |
| `build` | `warn build: this binary's build id is unavailable` | The binary cannot hash itself. |
| `build` | `warn build: the daemon reports no build id` | The identity reply holds no build. |

When the `daemon` check finds a stale socket, an untrusted socket or an identity failure, it MUST add the last 20 lines of `<home>/daemon.log`. Each line MUST read `warn daemon-log: <line>`. The command MUST read at most the last 64 KiB of the file. An absent, unreadable or empty log MUST give one `warn daemon-log` row and MUST NOT change the original failure.

| Check | Row | Meaning |
| --- | --- | --- |
| `threads` | `ok threads: <n> threads` | Each thread directory that holds `journal.jsonl` counts once. |
| `threads` | `warn threads: <n> threads, <m> damaged: <ids>` | The ids are the first five threads with a `damaged` marker. |
| `threads` | `fail threads: read sessions: <error>` | The sessions directory cannot be read. |

The `providers` check MUST write one row for each provider that has a stored or environment credential. The providers are the built-in providers and the `[providers.<id>]` entries of the config.

| Check | Row | Meaning |
| --- | --- | --- |
| `providers` | `ok providers: <id>: api key set` | The credential is an API key. |
| `providers` | `ok providers: <id>: expires in <duration>` | The token has not expired. |
| `providers` | `ok providers: <id>: token set, no expiry recorded` | The token has no expiry. |
| `providers` | `warn providers: <id>: expired, refreshes on next use` | The token expired and a refresh token exists. |
| `providers` | `fail providers: <id>: expired; run dalgona login <id>` | The token expired and no refresh token exists. |
| `providers` | `fail providers: <id>: expiry "<text>" is not valid; run dalgona login <id>` | The expiry is not an RFC 3339 time. |
| `providers` | `fail providers: <id>: api key is empty; run dalgona login <id>` | The stored key holds no text. |
| `providers` | `fail providers: <id>: <error>` | The store cannot read the credential. |
| `providers` | `warn providers: no credential found; run dalgona login <id>` | No provider has a credential. |

A duration MUST follow the duration form of [DESIGN.md](../../../DESIGN.md).

| Check | Row | Meaning |
| --- | --- | --- |
| `sandbox` | `ok sandbox: Landlock` | The Linux harness is available. |
| `sandbox` | `ok sandbox: sandbox-exec` | The macOS harness is available. |
| `sandbox` | `ok sandbox: off by configuration` | The config sets `sandbox = "off"`. |
| `sandbox` | `warn sandbox: <cause>; commands ask for a sandbox choice` | The harness is not available. |

The `sandbox` check MUST inspect the available mechanism only. It MUST NOT run a sandboxed command.

| Check | Row | Meaning |
| --- | --- | --- |
| `tls` | `ok tls: system roots` | The config sets no `tls_ca_file`. |
| `tls` | `ok tls: <n> certificates from <path>` | The file holds `<n>` certificates. |
| `tls` | `fail tls: <error>` | The file cannot be read or holds no certificate. |
| `tls` | `warn tls: not checked; the config did not load` | The `config` check failed. |

| Check | Row | Meaning |
| --- | --- | --- |
| `mcp` | `ok mcp: none configured` | The config holds no `[mcp_servers.<name>]` entry. |
| `mcp` | `ok mcp: <name>: stdio` | The server starts a command. |
| `mcp` | `ok mcp: <name>: http` | The server uses a URL. |
| `mcp` | `warn mcp: not checked; the config did not load` | The `config` check failed. |

The `mcp` check MUST NOT send `initialize` to a server.

The `plugins` check MUST load each `<home>/plugins/*.star` file the way the daemon loads it. The load runs the top level of the file and records the registrations. A top-level effect is an error. The check MUST NOT emit a hook and MUST NOT run a plugin tool. The check MUST NOT read a plugin file from a repository.

| Check | Row | Meaning |
| --- | --- | --- |
| `plugins` | `ok plugins: <n> loaded` | Each plugin file loads. |
| `plugins` | `fail plugins: plugin <file>: effects are not available during inspection` | The top level of the file calls an effect. |
| `plugins` | `fail plugins: plugin <file>: <error>` | The file does not compile or register. |
| `plugins` | `fail plugins: list plugins: <error>` | The plugin directory cannot be listed. |
