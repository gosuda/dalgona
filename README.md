# dal and dalgona

## Identity

`dal` (달, "moon") is the minimal agent and terminal interface. `dalgona` (달고나, the Korean sugar candy) is the batteries-included harness built on the same core. One brand family, two products.

Use lowercase `dal` and `dalgona` in running text. The executable names are `dal` and `dalgona`. Keep headings in sentence case and actions verb-first. Errors name what happened and how to fix it. Use no exclamation marks or moon puns in errors and status text. Keep any moon motif to at most one visual mark on a user-facing surface.

## Choose a product

Use dal for the agent and terminal interface without a daemon. Use dalgona for a per-user daemon, durable Starlark workflows, remote runners, MCP tools, a web interface, and Codex desktop integration. Both use the same agent core and Codex-compatible client protocol. An optional advisor reviews a thread and sends notes. It does not approve actions.

Sessions use an append-only JSONL journal with a SQLite mirror. Recovery follows recorded tool outcomes rather than assuming that an interrupted command did nothing. See [recovery](docs/spec/recovery.md) for the cases that can resume and those that must report interruption.

## Build from this checkout

Install the Go toolchain declared in [go.mod](go.mod), then run from the repository root:

```sh
go install ./cmd/...
dal --version
dalgona --version
```

Go installs both binaries in `GOBIN`, or in `GOPATH/bin` when `GOBIN` is unset. Add that directory to `PATH` if your shell cannot find them.

Use `just dist` to build release binaries for Linux, macOS, and Windows on amd64 and arm64. `just box-image` builds the remote box image from those binaries.

## Start dal

Start the terminal interface:

```sh
dal
```

In the terminal interface, run `/login <provider>` to sign in, then `/model` to select an offered model. Replace `<provider>` with your provider id, such as `openai` or `anthropic`. API-key providers can use their provider environment variables. No default thread model is shipped.

Enter a task in the composer. Use `/resume` to select an existing session, `/new` to start another, and Esc to interrupt a turn. [The TUI contract](docs/spec/tui.md) lists the commands and keys.

Dal uses `$DAL_HOME`, or `~/.dal` when the variable is unset. On Windows, dal sets a private access list on this directory for your user account, SYSTEM, and Administrators.

## Start dalgona

Start the daemon-backed terminal interface:

```sh
dalgona
```

The client starts the user daemon when needed and connects to it. Use `/login <provider>` and `/model` in the terminal interface. Reopen a recorded thread with:

```sh
dalgona --resume <thread-id>
```

For login-time startup, use `dalgona service install`. Use `dalgona service uninstall` to remove that startup configuration. The platform service rules are in [daemon.md](docs/spec/daemon.md).

Dalgona uses `$DALGONA_HOME`, or `~/.dalgona` when the variable is unset. It does not share dal's home. On Windows, dalgona sets a private access list on this directory for your user account, SYSTEM, and Administrators: daemon start resets the list on the home and everything inside it, and a client refuses to start when it cannot protect the home.

## Configure a product

Put user configuration in `<home>/config.toml`. The program stores mutable settings in `<home>/settings.toml`. Settings override config, which overrides defaults. Client flags such as `-m`, `--approval`, and `--sandbox` set thread options. `--remote` overrides the configured `remote`. Unknown config keys fail loading.

Both products accept this configuration:

```toml
approval = "auto"
sandbox = "workspace-write"

[approval_decider]
decider = "decisions:typesafe/jev-1.14"
threshold = 0.9

[advisor]
enabled = false
```

`auto` uses static rules and an automated decider. A missing or failed decider declines actions that need its decision. Use `ask` for human approval. Use `yolo` only when you intend to bypass the decider and turn off harness confinement.

Set `enabled = true` in `[advisor]` to request transcript reviews. Use `[advisors.<name>]` for named reviewers. There is no `--advisor` CLI flag. [The advisor contract](docs/spec/advisor.md) defines model selection, review timing, and note delivery.

Use `[approval_decider]`, not `[approval]`. TOML cannot use `approval` as both a scalar and a table. See [configuration](docs/spec/config.md) for keys, provider endpoints, edit styles, and product restrictions.

Use these table names and keys:

| Scope | Keys |
| --- | --- |
| Both products, top level | `model`, `reasoning`, `approval`, `sandbox`, `sandbox_writable_roots`, `edit_style`, `compaction_threshold`, `compaction_keep_tokens`, `stream_start_timeout`, `stream_idle_timeout` |
| `[providers.<name>]` | `base_url`, `api_key_env` |
| `[models.<name>]` | `context_window` |
| `[approval_decider]` | `decider`, `threshold` |
| `[roles]` | `smol`, `slow`, `advisor` |
| `[advisor]` | `enabled`, `review`, `interval`, `sync`, `continue_severity`, `immune_turns`, `max_notes`, `tools`, `subagents` |
| `[advisors.<name>]` | `model`, `instructions`, `review`, `interval`, `sync`, `tools`, `subagents`, `enabled` |
| `[secrets]` | `patterns` |
| `[keywords.<name>]` | `notice`, `text`, `hue`, `effort`, `requires`, `enabled` |
| Dalgona only, top level | `tool_mode`, `runner`, `remote` |
| `[runners.<name>]` (dalgona) | `service`, `image`, `template`, `url` |
| `[remotes.<name>]` (dalgona) | `service`, `image`, `template`, `url`, `box_id` |
| `[mcp_servers.<name>]` (dalgona) | `command`, `args`, `env`, `url` |
| `[discovery]` (dalgona) | `sources` |
| `[eval]` (dalgona) | `uses` |
| `[herdr]` (dalgona) | `subagent_panes` |
| `[web]` (dalgona) | `listen`, `token`, `cert_file`, `key_file` |

`edit_style` accepts a style name or a table of model globs with a required `default` entry. Styles are `anchor`, `replace`, `hashline`, `hashline-light`, `hashline-enhanced`, `apply_patch`, `simple`, `balanced`, and `strict`. Model globs match in declaration order. Dal rejects the dalgona-only settings.

## Run inside an outer sandbox

Run dalgona inside a lightweight outer sandbox, such as a container, microVM, or cloud sandbox runner. The local harness sandbox only confines accidental writes and is not a security boundary. See [sandbox.md](docs/spec/sandbox.md) for the write policy. On Linux, when the local write policy applies, a `dalgona` command that a sandboxed command starts is refused by the daemon. On macOS, the sandbox profile denies access to the daemon socket. A daemon that itself runs with `no_new_privs` logs a warning, and sandboxed commands can still reach it.

Windows has no harness confinement. Linux runs without it when Landlock is unavailable. A remote box supplies the sandbox instead of the local write policy. See [sandbox.md](docs/spec/sandbox.md).

## Connect the Codex desktop app

Install the desktop override:

```sh
dalgona codex install
```

Restart the Codex desktop app after installation. The installer records prior settings before it changes the user-level executable override. Use `dalgona codex status` to inspect the override and `dalgona codex uninstall` to restore it. The integration does not patch the signed app bundle. See [Codex app integration](docs/spec/codex-app.md).

`dalgona app-server` bridges the desktop protocol. Its default listener is standard input and output. The app-server and the terminal interface use the same daemon protocol.

## Choose where execution runs

### Keep the orchestrator local

Set `runner` to a named entry in dalgona's config. For example:

```toml
runner = "devbox"

[runners.devbox]
service = "docker"
```

The runner uploads the workspace, records its baseline, and runs tools and subagents in the box. The supported services are in [runners.md](docs/spec/runners.md). A fork shares its parent's box. Archiving one thread does not destroy a box still used by another thread.

Use `dalgona pull <thread-id>` to apply the box's changes to the local workspace with `git apply --3way`. Conflicts leave markers and return exit code 1 with the affected files. `dalgona runner gc` lists unused boxes without deleting them. Use `dalgona runner gc --yes` to delete them.

### Run the whole daemon in a box

Configure `[remotes.<name>]`, then run:

```sh
dalgona cloud up <name>
dalgona --remote <name>
dalgona cloud pull <name>
dalgona cloud down <name>
```

The named remote uses any service in [runners.md](docs/spec/runners.md) except Cloudflare. Cloudflare cannot host a durable whole-daemon home because its containers lose their files on stop.

`cloud up` does not copy credentials unless you pass `--copy-auth`. Without credential copying, use `dalgona --remote <name> login` to authenticate in the box. Remote service and box identifiers live in `[remotes.<name>]` in `settings.toml`.

`cloud down` destroys the box and its journal after confirmation. `--yes` skips that confirmation for scripts. Pull changes you need before deletion.

Use `--remote <name>` with the terminal client, `web`, or `app-server` to connect to the in-box daemon through its bridge. See [the service decision](docs/decisions/sandbox-services.md) and [exec-server wire](docs/spec/exec-server.md) for persistence and transport rules.

## Other product commands

Both products accept these client commands and flags:

| Command or flag | Purpose |
| --- | --- |
| `dal`, `dalgona` | Open the terminal interface. |
| `login [provider]` | List providers or sign in to one. |
| `logout <provider>` | Remove a stored credential. |
| `models` | List offered models. |
| `-p <prompt>` | Run one turn and exit. |
| `--json` | With `-p`, print one JSON status line. |
| `--resume <thread-id>` | Resume a recorded thread. |
| `-c` | Continue the latest thread; cannot combine with `--resume`. |
| `-m <model-id>` | Select the model for a new thread. |
| `--approval auto\|ask\|yolo` | Set the approval mode. |
| `--sandbox workspace-write\|off` | Set the harness sandbox mode. |
| `-C <directory>` | Change the working directory. |
| `--help`, `--version` | Print client flag help or the build version. |

Prefix client commands with the product name, for example `dal models` or `dalgona -p "Explain this repository"`. Dalgona also accepts `--remote <name>` to select a configured remote instead of the local daemon.

These commands are specific to dalgona:

| Command | Purpose |
| --- | --- |
| `dalgona daemon` | Run the per-user daemon. |
| `dalgona app-server [--listen <address>]` | Bridge the desktop protocol over standard input and output or a loopback WebSocket. |
| `dalgona service install\|uninstall` | Add or remove login-time daemon startup. |
| `dalgona pull <thread-id>` | Apply a thread box's changes locally. |
| `dalgona runner gc [--yes]` | List unused boxes, or delete them with `--yes`. |
| `dalgona cloud up <name> [--copy-auth]` | Create a whole-daemon box. |
| `dalgona cloud pull <name>` | Apply a whole-daemon box's workspace changes locally. |
| `dalgona cloud down <name> [--yes]` | Destroy a whole-daemon box. |
| `dalgona codex install\|uninstall\|status` | Manage the desktop executable override. |
| `dalgona codex record --real <codex-path> [args...]` | Record protocol metadata while running the selected Codex executable. |
| `dalgona web` | Serve the web interface; set its listener, token, and TLS files in `[web]`, not CLI flags. See [web access rules](docs/spec/web.md). |
| `dalgona run <file.star> [--arg k=v ...]` | Run a durable Starlark workflow. A value that is valid JSON is passed as JSON; any other value is passed as a string. |
| `dalgona exec-server [--listen <address>] [--root <directory>] [--data <directory>]` | Serve process and filesystem operations inside a box. |
| `dalgona bridge` | Connect standard input and output to the local daemon, starting it if needed. |
| `just dist` | Build release binaries for remote bootstrap. |
| `just box-image` | Build the remote box image. |

`app-server` defaults to `stdio://`. Its WebSocket listener accepts `ws://<loopback-address>:<port>`. Use repeated `-c key=value` or `--config key=value` arguments to set `model`, `approval_policy`, or `sandbox_mode` for new desktop threads.

`exec-server` defaults to `--listen stdio:// --root /workspace`. Its WebSocket listener accepts `ws://IP:PORT` and requires `DALGONA_BOX_TOKEN` with at least 32 characters. `--data` selects a persistent data directory. The bridge and exec server run where you invoke them; `--remote` does not redirect them.

`codex record` defaults to forwarding `app-server` when you omit arguments. It writes protocol metadata to `<home>/census/<unix-seconds>.jsonl`.

## Read the contracts and contribute

[The documentation index](docs/README.md) links every current specification and decision. Start with [journal](docs/spec/journal.md), [recovery](docs/spec/recovery.md), and [protocol](docs/spec/protocol.md) for the core contracts.

Read [CONTRIBUTE.md](CONTRIBUTE.md) before changing code and [REVIEW.md](REVIEW.md) before reviewing it. [AGENTS.md](AGENTS.md) contains repository instructions for coding agents. [justfile](justfile) declares the available checks. `just verify` runs the Go, web, and Cloudflare checks. It also runs the hashline model check when `uv` is installed.
