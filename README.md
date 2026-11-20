# dalgona (/dal)

<img src="assets/brand/dalgona.svg" width="80" align="right" alt="The dalgona mark: a caramel candy stamped with a crescent moon and a rabbit.">

Terminal coding agents, with or without a daemon.

`dal` runs the agent host inside your terminal process and needs no daemon. `dalgona` connects terminal, editor, and browser clients to a per-user daemon and adds a web interface, remote runners, MCP tools, an Agent Client Protocol endpoint, and durable Starlark workflows. Both commands share one agent core and keep their own homes and threads, so you can start with `dal` and adopt `dalgona` when you need it.

[![Built with Depot](https://depot.dev/badges/built-with-depot.svg)](https://depot.dev?utm_source=Mealie)

## Quick start

Install both commands with the Go toolchain declared in [go.mod](go.mod):

```sh
go install github.com/gosuda/dalgona/cmd/dal@latest github.com/gosuda/dalgona/cmd/dalgona@latest
dal --version
# dal v0.0.0-<timestamp>-<commit>
```

With no release tag, `@latest` resolves to the head of the default branch. Pin an exact build with `@<commit-hash>`.

The commands land in the Go binary directory (`$GOBIN` when set, otherwise the `bin` directory under the first entry of `go env GOPATH`). If that directory is not on `PATH`, the install succeeds but `dal --version` fails; add it to `PATH` before continuing.

Open the terminal interface from the directory you want the agent to work in:

```sh
dal
```

In the terminal interface:

1. Enter `/login` and choose a provider.
2. Complete its sign-in flow.
3. Enter `/model` and choose an offered model.
4. Type a task and press Enter.

API-key providers (`openai`, `anthropic`, `google`, `openrouter`) take a pasted key in the sign-in panel and also read their provider environment variables. In the panel, only `chatgpt` offers a browser link or a device code. `anthropic` takes an API key there and runs OAuth in a shell (`dal login anthropic`). `copilot`, `cursor`, `devin`, and `antigravity` sign in only in a shell (`dal login <provider>`).

To use the daemon-backed product instead, run:

```sh
dalgona
```

It starts the per-user daemon when needed. Sign in and choose a model there too, because the products use separate homes.

## Features

- Work in the terminal with either product, in a browser with `dalgona web`, or through an editor with `dalgona acp`.
- Resume recorded threads and inspect their tool outcomes. Threads use an append-only JSONL journal with a Pebble mirror, and [recovery](docs/dal/spec/recovery.md) reuses recorded tool outcomes after an interruption.
- Extend `dalgona` with Starlark. Plugins from `<home>/plugins/*.star` add tools, hooks, and commands; `dalgona run` executes durable workflows. See the [Starlark contract](docs/dal/spec/starlark.md).
- Choose how actions are approved: static rules plus an automated decider with `auto`, human approval with `ask`, or `yolo`. An optional advisor reviews a thread and sends notes without approving actions.
- Run `dalgona` tools and subagents in a remote box while the orchestrator stays local, or move the whole daemon into a box. See [remote runners](docs/dalgona/spec/runners.md).
- On `dalgona`, attach MCP servers, sign in to OAuth MCP servers, connect the Codex desktop app, and check the whole setup with `dalgona doctor`.

## Install

The `go install` commands in [Quick start](#quick-start) are the only install step for the terminal products. For a fresh copy:

```sh
git clone https://github.com/gosuda/dalgona.git
cd dalgona
export GOBIN="$PWD/bin"
go install ./cmd/...
export PATH="$GOBIN:$PATH"
```

From the root of a checkout you already have, skip the clone:

```sh
export GOBIN="$PWD/bin"
go install ./cmd/...
export PATH="$GOBIN:$PATH"
```

This installs both commands into that checkout's `bin/` directory. A build from a checkout reports `(devel)` as its version; a `go install @latest` build reports the module pseudo-version. Keep the install directory on `PATH` in later shells.

The terminal commands do not need the web build. To include the web interface, install `just`, Node.js (the version in [CI](.latchkey/workflows/ci.yml)), and the pnpm version declared in [web/package.json](web/package.json). Then build the assets and reinstall:

```sh
just web
go install ./cmd/...
```

Without built assets, `dalgona web` serves a message that the web UI is not built.

## Usage

### Work in the terminal

Use `/resume` to select a recorded thread and `/new` to start another. Press Esc twice to interrupt a turn. The [terminal contract](docs/dal/spec/tui.md) lists the commands and keys.

Resume a thread by its identifier:

```sh
dalgona --resume <thread-id>
```

Both products also accept `-c` to continue the latest thread. Do not combine it with `--resume`.

After choosing a model, run a single prompt without the terminal interface:

```sh
dal -p "Explain this repository"
dalgona -p "Explain this repository"
```

Use `dal login` or `dalgona login` to list sign-in targets, and `dal models` or `dalgona models` to list offered models.

### Connect an ACP editor

```sh
dalgona acp
```

It serves the [Agent Client Protocol](docs/dalgona/spec/daemon.md) on standard input and output for editors that speak ACP. Each ACP session maps to one daemon thread.

### Keep the daemon running at login

```sh
dalgona service install
```

Use `dalgona service uninstall` to remove login-time startup. The [daemon contract](docs/dalgona/spec/daemon.md) describes the platform service rules.

### Open the web interface

After building the web assets and reinstalling the binaries, run:

```sh
dalgona web
```

Open the URL that the server prints. Configure its listener, token, and TLS files in `[web]`, not command-line flags. See the [web access rules](docs/dalgona/spec/web.md).

### Connect the Codex desktop app

```sh
dalgona codex install
```

Restart the desktop app after installation. The installer records prior settings before changing the user-level executable override. It does not patch the signed app bundle.

Use `dalgona codex status` to inspect the override and `dalgona codex uninstall` to restore prior settings. See [Codex app integration](docs/dalgona/spec/codex-app.md).

`dalgona app-server` bridges the desktop protocol. It uses standard input and output by default and connects through the same daemon protocol as the terminal client.

### Keep the orchestrator local

Add a named runner to dalgona's configuration:

```toml
runner = "devbox"

[runners.devbox]
service = "docker"
```

The runner uploads the workspace, records its baseline, and runs tools and subagents in the box. A fork shares its parent's box. Archiving a thread keeps a box that another thread still uses.

Use `dalgona pull <thread-id>` to apply the box's changes locally. In a Git work tree, pull uses `git apply --3way`. Conflicts return exit code 1 and list affected files. Outside a Git work tree, pull uses `git apply`.

`dalgona runner gc` lists unused boxes without deleting them. Use `dalgona runner gc --yes` to delete them. The [runner contract](docs/dalgona/spec/runners.md) lists supported services and persistence rules.

### Run the whole daemon in a box

Configure `[remotes.<name>]` with a supported service, then replace `<name>` below with that entry's name:

```sh
dalgona cloud up <name>
dalgona --remote <name>
dalgona cloud pull <name>
dalgona cloud down <name>
```

Whole-daemon mode accepts the services in [runners.md](docs/dalgona/spec/runners.md) except Cloudflare. Cloudflare cannot keep a durable daemon home across container stops.

`cloud up` copies the whole local `auth.json`, including stored API keys, only with `--copy-auth`. Without that flag, use `dalgona --remote <name> login` for API-key providers and `chatgpt`. For `antigravity`, `cursor`, or `devin`, sign in locally, then run `dalgona cloud up <name> --copy-auth`.

Remote service and box identifiers live in `[remotes.<name>]` in `settings.toml`. Use `--remote <name>` with the terminal client, `web`, or `app-server` to connect through the box's bridge.

> `cloud down` destroys the box and every thread in it after confirmation. Pull any changes you need first. Its `--yes` flag skips confirmation.

See the [sandbox service decision](docs/dalgona/decisions/sandbox-services.md) and [exec-server contract](docs/dal/spec/exec-server.md) for transport and persistence details.

### Check the setup

```sh
dalgona doctor
# ok home: /home/<you>/.dalgona
# ok config: valid
# ok sandbox: Landlock
# ok tls: system roots
# ok mcp: none configured
# ok plugins: 0 loaded
```

Each check prints one `ok`, `warn`, or `fail` row. `--online` reads one model list per credentialed provider doctor can list: most providers answer one live request, `antigravity` reports its bundled catalog with no request, and a credentialed provider doctor cannot list (such as `copilot`) gives a `warn` row with no request. A `[providers.<id>]` entry replaces the built-in adapter, so with such an entry `doctor --online` sends one live request to that entry's `base_url`, or to OpenAI's endpoint (`https://api.openai.com/v1`) when the entry sets no `base_url`, including for `antigravity` or `copilot`. See the [doctor contract](docs/dalgona/spec/doctor.md) for the full row list.

### Use an outer sandbox

Run dalgona inside a container, microVM, or cloud sandbox runner when you need an outer boundary. The local harness sandbox limits accidental writes. It is not a security boundary.

Windows has no harness confinement. Linux has none when Landlock is unavailable. macOS has none when `sandbox-exec` is unavailable. With `workspace-write`, an agent call first asks how to proceed without confinement. Without a client to answer, it declines.

On Linux, the daemon refuses peers with `no_new_privs` unless it has that mark itself. On macOS, the harness profile denies access to the daemon socket. A daemon started with `no_new_privs` warns that sandboxed commands can still reach it.

A remote box supplies the sandbox instead of the local write policy. See [sandbox.md](docs/dal/spec/sandbox.md) for limits and fallback rules.

## Commands and options

### Shared client commands

Prefix these commands and flags with `dal` or `dalgona`.

| Command or flag | Purpose |
| --- | --- |
| No arguments | Open the terminal interface. |
| `login [provider]` | List providers or sign in to one. |
| `logout <provider>` | Remove a stored credential. |
| `models` | List offered models. |
| `-p <prompt>` | Run one turn and exit. |
| `--json` | With `-p`, print one JSON status line. |
| `--resume <thread-id>` | Resume a recorded thread. |
| `-c` | Continue the latest thread. |
| `-m <model-id>` | Select the model for a new thread. |
| `--approval auto\|ask\|yolo` | Set the approval mode. |
| `--sandbox workspace-write\|off` | Set the harness sandbox mode. |
| `-C <directory>` | Change the working directory. |
| `--help`, `--version` | Print client flag help or the build version. |

Dalgona also accepts `--remote <name>` to select a configured remote instead of the local daemon.

### Dalgona commands

| Command | Purpose |
| --- | --- |
| `dalgona daemon` | Run the per-user daemon. |
| `dalgona restart` | Restart the running daemon, for example after a build mismatch. |
| `dalgona app-server [--listen <address>]` | Bridge the desktop protocol over standard input and output or a loopback WebSocket. |
| `dalgona acp` | Serve the Agent Client Protocol on standard input and output. |
| `dalgona service install\|uninstall` | Add or remove login-time daemon startup. |
| `dalgona pull <thread-id>` | Apply a thread box's changes locally. |
| `dalgona runner gc [--yes]` | List unused boxes, or delete them with `--yes`. |
| `dalgona cloud up <name> [--copy-auth]` | Create or attach a whole-daemon box. |
| `dalgona cloud pull <name>` | Apply a whole-daemon box's workspace changes locally. |
| `dalgona cloud down <name> [--yes]` | Destroy a whole-daemon box. |
| `dalgona codex install\|uninstall\|status` | Manage the desktop executable override. |
| `dalgona codex record --real <codex-path> [args...]` | Record protocol metadata while running the selected Codex executable. |
| `dalgona web` | Serve the built web interface. |
| `dalgona run <file.star> [--arg k=v ...]` | Run a durable Starlark workflow. |
| `dalgona exec-server [--listen <address>] [--root <directory>] [--data <directory>]` | Serve process and filesystem operations inside a box. |
| `dalgona bridge` | Connect standard input and output to the local daemon, starting it if needed. |
| `dalgona mcp login <name>` | Sign in to an OAuth MCP server and store its token. |
| `dalgona failures reporting on\|off\|login` | Consent to, disable, or authorize automatic failure reports. |
| `dalgona doctor [--online]` | Print one `ok`, `warn` or `fail` row for each setup check. `--online` adds one model-list request for each provider with a credential that has a model list to read. |

Workflow arguments use JSON when the value is valid JSON. Other values become strings.

`app-server` defaults to `stdio://`. Its WebSocket listener accepts `ws://<loopback-IP>:<port>`, not a hostname. Repeated `-c key=value` or `--config key=value` arguments supply defaults for `model`, `approval_policy`, or `sandbox_mode` in new desktop threads.

`exec-server` defaults to `--listen stdio:// --root /workspace`. Its WebSocket listener accepts `ws://IP:PORT` and requires `DALGONA_BOX_TOKEN` with at least 32 characters. `--data` selects a persistent data directory.

`bridge` and `exec-server` run where you invoke them. `--remote` does not redirect them.

`codex record` forwards `app-server` when you omit forwarded arguments. It writes protocol metadata to `<home>/census/<unix-seconds>.jsonl`.

## Configuration

### Choose a home

| Product | Home override | Default home |
| --- | --- | --- |
| `dal` | `DAL_HOME` | `~/.dal` |
| `dalgona` | `DALGONA_HOME` | `~/.dalgona` |

Put user configuration in `<home>/config.toml`. The program stores mutable settings in `<home>/settings.toml`.

Settings override configuration, which overrides defaults. Client flags such as `-m`, `--approval`, and `--sandbox` set thread options. Dalgona's `--remote` overrides the configured `remote`. Unknown configuration keys fail loading.

On Windows, each host protects its home for the current user, SYSTEM, and Administrators. Dalgona resets that access list on the home and its contents at daemon start. Its client refuses to start when it cannot protect the home. See [configuration](docs/dal/spec/config.md) and [daemon.md](docs/dalgona/spec/daemon.md) for the exact rules.

### Set approval and review

Both products accept:

```toml
approval = "auto"
sandbox = "workspace-write"

[approval_decider]
decider = "decisions:typesafe/jev-1.14"
threshold = 0.9

[advisor]
enabled = false
```

- `auto` uses static rules and an automated decider. A missing or failed decider declines actions that need its decision.
- `ask` requests human approval.
- `yolo` bypasses the decider and turns off harness confinement.

Use `[approval_decider]`, not `[approval]`. TOML cannot use `approval` as both a scalar and a table.

Set `[advisor] enabled = true` to request transcript reviews. Use `[advisors.<name>]` for named reviewers. There is no `--advisor` flag. The [advisor contract](docs/dal/spec/advisor.md) defines model selection, timing, and note delivery.

Dalgona also offers opt-in [edit review](docs/dal/spec/edit-review.md). It sends masked edit diffs to the review model. The feature is off by default.

### Configuration keys

| Scope | Keys |
| --- | --- |
| Both products, top level | `model`, `reasoning`, `approval`, `tool_mode`, `sandbox`, `sandbox_writable_roots`, `edit_style`, `compaction_threshold`, `compaction_keep_tokens`, `stream_start_timeout`, `stream_idle_timeout`, `tls_ca_file` |
| `[providers.<name>]` | `base_url`, `api_key_env` |
| `[models.<name>]` | `context_window`, `input_usd_per_mtok`, `output_usd_per_mtok`, `cache_read_usd_per_mtok`, `cache_write_usd_per_mtok` |
| `[approval_decider]` | `decider`, `threshold` |
| `[roles]` | `smol`, `slow`, `advisor` |
| `[advisor]` | `enabled`, `review`, `interval`, `sync`, `continue_severity`, `immune_turns`, `max_notes`, `tools`, `subagents` |
| `[advisors.<name>]` | `model`, `instructions`, `review`, `interval`, `sync`, `tools`, `subagents`, `enabled` |
| `[secrets]` | `patterns`, `builtin` |
| `[limits]` | `max_subagent_starts` |
| `[keywords.<name>]` | `notice`, `text`, `hue`, `effort`, `requires`, `enabled` |
| `[eval]` | `uses` |
| Dalgona only, top level | `runner`, `remote` |
| `[runners.<name>]` (dalgona) | `service`, `image`, `template`, `url` |
| `[remotes.<name>]` (dalgona) | `service`, `image`, `template`, `url`, `box_id` |
| `[mcp_servers.<name>]` (dalgona) | `command`, `args`, `env`, `url`, `bearer_token_env`, `oauth`, `oauth_scopes`, `oauth_client_id`, `oauth_callback_port` |
| `[daltoki]` (dalgona) | `enabled`, `app_id`, `private_key_file`, `repositories`, `model`, `profile`, `poll_interval_seconds`, `concurrency`, `jobs_per_day`, token and cost limits, relay keys |
| `[discovery]` (dalgona) | `sources` |
| `[edit_review]` (dalgona) | `enabled`, `feedback` |
| `[herdr]` (dalgona) | `subagent_panes` |
| `[web]` (dalgona) | `listen`, `token`, `cert_file`, `key_file` |

`edit_style` accepts a style name or a table of model globs with a required `default` entry. Styles are `anchor`, `replace`, `hashline`, `hashline-light`, `hashline-enhanced`, `apply_patch`, `simple`, `balanced`, and `strict`. Model globs match in declaration order.

Both products default to `tool_mode = "eval-first"`. Use `"normal"` for the full tool list. Use `"eval-only"` for the restricted direct tool list. Dal rejects dalgona-only settings.

See [config.md](docs/dal/spec/config.md) for all settings, including verbosity and themes. The [Starlark contract](docs/dal/spec/starlark.md) defines tool visibility and which calls need a direct user interaction.

## Documentation

The [documentation index](docs/README.md) links every specification and decision. Start with [journal](docs/dal/spec/journal.md), [recovery](docs/dal/spec/recovery.md), and [protocol](docs/dal/spec/protocol.md) for the core contracts, and [runners](docs/dalgona/spec/runners.md) and [daemon](docs/dalgona/spec/daemon.md) for the dalgona batteries.

## Development

Use the Go toolchain in [go.mod](go.mod) and the Node.js and pnpm versions declared in [CI](.latchkey/workflows/ci.yml). Install `just` and `uv` for the checks.

From the checkout root:

```sh
just web
just build
just verify
```

`just verify` runs formatting checks, vet, lint, race-enabled tests, vulnerability checks, the hashline model, specification style, web checks, and Cloudflare checks.

[CI](docs/dal/spec/ci.md) runs the shared checks once, then the Go checks on Linux, macOS, and Windows with the same built web assets.

Without `uv`, `hashline-model` and `spec-style` skip locally and fail when `CI=true`. A skipped check is not a pass.

| Command | Purpose |
| --- | --- |
| `just build` | Build both commands for the host into `bin/`. |
| `just dal [args...]` | Rebuild `dal` and run it with the given arguments. |
| `just dalgona [args...]` | Rebuild `dalgona` and run it with the given arguments. |
| `just dalgona --version` | Rebuild `dalgona` and print its version. |
| `just web` | Install locked web dependencies and build the web assets. |
| `just dist` | Build release binaries for Linux, macOS, and Windows on amd64 and arm64. |
| `just box-dist` | Build the Linux amd64 and arm64 binaries for the remote box image. |
| `just box-image` | Build the box binaries and remote box image with Docker Buildx. |

## Contributing

Read [CONTRIBUTE.md](CONTRIBUTE.md) before changing code and [REVIEW.md](REVIEW.md) before reviewing it. [AGENTS.md](AGENTS.md) contains repository instructions for coding agents. [justfile](justfile) declares the checks.

## License

The project uses the [Sustainable Use License, Version 1.0](LICENSE.md). It permits internal business, non-commercial, and personal use under its stated limits. Read the full license before distributing the software.

Third-party components retain their own licenses. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and the font terms in [LICENSE.md](LICENSE.md).
