# dal and dalgona

Dal is a complete terminal coding agent. Dalgona is dal with opinionated batteries, switched on through the same public extension door.

---

- [What is this](#what-is-this)
- [Install](#install)
- [Quick start](#quick-start)
- [Features](#features)
- [Usage](#usage)
- [Documentation](#documentation)
- [Development](#development)
- [License](#license)

## What is this

Dal works without dalgona. It ships the agent loop, providers, tools, Starlark extensions, terminal interface, session storage, manuals, and wire protocols in one product.

Dalgona reuses dal through the public extension interface and turns on batteries for orchestration, history, quality, judged rules, web, work, MCP, review, ask, skills, ttsr-rules, and procs. It keeps its own configuration, data, and sessions.

Binaries are `dalgon` (aliases `dal`, `dl`) and `dalgona` (alias `dg`). A plugin that loads in dalgon loads in dalgona.

Everything extends dal through one door. In process, the Host and Agent seam carries one snapshot, one change stream, one command path, one request and answer pair, and one blob read. Across processes, the same operations travel as JSON-RPC over stdio or a local socket. The manuals live behind that door at `dal://` and `dalgona://`.

## Install

In a POSIX shell, build from source. The install guide in this checkout describes a source route and lists no registry version to install.

You need git, the latest stable Rust toolchain, and a C toolchain. The symbol grammars compile through `cc`.

```sh
git clone https://github.com/gosuda/dalgona
cd dalgona
rustup update stable --no-self-update
cargo build --release --locked -p dalgon -p dalgona
export PATH="$PWD/target/release:$PATH"
```

The binaries land in `target/release/` (`dalgon` with `dal` and `dl`, plus `dalgona` with `dg`), assuming the default Cargo target directory (`CARGO_TARGET_DIR` and `--target-dir` move it). The `export` line assumes POSIX `sh`; PowerShell needs different syntax to extend `PATH`.

Supported platforms are Linux, macOS, and Windows on x86_64 and aarch64. Unsigned Windows binaries may trigger SmartScreen; verify the checksum where one is published. Configuration and data use per-OS roots; on POSIX defaults these are `~/.config/dal/` and `~/.local/share/dal/`, and dalgona uses its own family directories. Plugins need no toolchain.

## Quick start

The outputs below summarize checked-in CLI help and docs.

```sh
dalgon docs
# prints the dal manual index starting with `# dal://`
dalgon docs dal://cli
# prints the command-line page titled `The command line`
dalgon login --help
# prints sign-in usage starting with `Sign in to a provider`
dalgon login status
# exit 1 with no credentials; lists providers as `not configured`
```

Run `dal` for the terminal once a provider is ready. Sign in with `dalgon login`, then pick a model.

## Features

- **Complete alone**: chat in the terminal, search and edit with approval, keep journals, and read `dal://` without installing dalgona.
- **Same door**: load any dal plugin in dalgona unchanged. Name a dalgona battery in `disabled_batteries` to leave it out.
- **Starlark plugins**: ship tools, commands, hooks, skills, rules, and prompt text from one `plugin.star` with no compile step. A broken plugin stops startup and names `path:line:col`. `/reload` keeps the previous set on failure.
- **Approval first**: ask before every `patch` and every `exec` by default. Headless runs deny when no person can answer. `eval` runs Starlark only and is not a sandbox.
- **Durable sessions**: continue with `-c`, resume with `-r`, branch with `/fork` and `/clone`, and share with `/export`. Compaction starts at 85 percent of the context window.
- **Terminal and headless**: work inline or fullscreen, print with `-p`, emit JSON with `--json`, or drive ACP, RPC, router, and A2A clients over one protocol.
- **Batteries on demand**: run dalgona for orchestration, plan and todo, ask, web search and fetch, MCP client, review, judged rules, quality guard, history, skills, ttsr-rules, symbol search, hashline edits, and the deferred `procs` tool over background jobs.

### Scope limits

Dal keeps its core small. It has no MCP client (dalgona ships one; a plugin can reach HTTP services with `net.fetch`), no plan or todo (dalgona ships both), and no subagents by default. There is no LSP in either product, no upload (`/export` writes a file you share yourself), no self-rebuild (`/reload` reloads plugins), nothing but Starlark in `eval`, no second store besides the journal, and no browser front end in the binary. The optional sandbox is off by default, guards the home directory, and sets no network restriction. A plugin runs with your permissions, so read it before you install it.

## Usage

### Sign in and list models

```sh
dalgon login
dalgon login status
dalgon models
dalgon logout
```

`login` signs in interactively or with `--api-key` or `--device-auth` where the provider supports it. `login status` exits 0 when at least one provider is ready and 1 otherwise, with a sign-in hint and no secret output. `models` lists models with built-in prices and sources. `logout` removes stored credentials. Secrets live in `auth.json` with mode `0600`; the loader refuses symlinks and group or other permission bits.

### Read the manual

```sh
dalgon docs
dalgon docs dal://cli
dalgona docs dalgona://batteries
```

With no URI, `docs` prints the index. With a page URI, it prints the page. Unknown URIs fail with a miss message on stderr and nothing on stdout.

### Run in the terminal

```sh
dal
dal --screen fullscreen
dalgona
```

`dal` starts the inline terminal by default once a provider and model are set. `--screen fullscreen` uses the alternate screen and replays the settled transcript on exit. `dalgona` starts the batteries product with its own config, data, and sessions. You can type while dal works; a steer reaches the model at the next safe point, a follow-up waits for the turn to end, and stop ends the turn at once.

### Run one turn without the TUI

```sh
dalgon -p "Summarize the staged diff"
dalgon --json -o reply.md "Summarize the staged diff"
dalgon -c -p "Continue with the next file"
dalgon -r my-session -p "Resume and finish"
```

These need a configured model; without one dalgon reports the missing model and how to set it. Print mode runs one turn and waits up to 3 seconds for extension status to go quiet. JSON mode writes one JSON result or error line on stdout (with the stop reason, not the assistant text) only after every status kind is quiet; pair it with `-o FILE` to save the assistant message. Common run flags are `-m` for model, `--thinking`, `--mode`, `-n` for name, `-C` for workspace directory, and `--no-session` to skip saving.

### Control sessions and safety

```sh
dalgon -p --approval ask "Inspect the build script"
dalgon -p --approval edits "Format and update the changelog"
dalgon -p --approval edits --sandbox "Run the test suite"
```

Pass `--approval` and `--sandbox` as run modifiers on a TUI or headless invocation (without a prompt, bare `dalgon --approval ...` starts the TUI on a terminal and fails on a pipe). The default `ask` asks before every `patch` and every `exec`. `edits` approves edits without asking and still asks for `exec`. `all` approves every call and is only for runs already confined by an external sandbox; the built-in `--sandbox` sets no network restriction, so it is not that confinement. When no controller can answer, dal denies and says why. `--sandbox` runs `exec` children through the platform helper with workspace, temporary, cache, and `sandbox_writable` roots writable. `dalgon serve` binds loopback only unless `--public` is given with a token from `dalgon serve token`.

### Manage plugins

```sh
dalgon plugin list
dalgona plugin grant quality
dalgona plugin revoke quality
```

User plugins load from the data root only, never from the workspace, and must be listed in `plugins` in `dal.toml` before `dalgon plugin grant <name>` can grant them. In `dalgona`, bundled batteries such as `quality` are already registered (`dalgon` alone rejects `dalgon plugin grant quality` as not configured). `list` shows configured plugins and persistent grants. `grant` and `revoke` change the stored grant; session grants end with their sessions.

### Serve and integrate

```sh
dalgon serve
dalgon serve token
dalgon serve --public
dalgon serve --a2a
dalgon rpc
dalgon rpc --socket /tmp/dalgon.sock
dalgon completion bash
dalgon rules test "check this text"
```

`serve` starts the local router; `--public` listens wider and requires a token, and `--a2a` enables the agent-to-agent surface. `rpc` serves JSON-RPC over stdio or a local socket. `completion` prints shell completions for `bash`, `elvish`, `fish`, `powershell`, and `zsh`. `rules test` simulates one stream rule against the given text with `--source`, `--tool`, and `--path` selectors.

## Documentation

The same pages live inside the binaries and in the checkout. Use `dalgon docs` for `dal://` and `dalgona docs` for `dalgona://`.

Core use:

- [Command line](docs/cli.md)
- [Slash commands](docs/commands.md)
- [Settings in dal.toml](docs/config.md)
- [Providers and models](docs/models.md)
- [Tools, modes, and approval](docs/tools.md)

Extension and session control:

- [Plugins](docs/plugins.md)
- [Rules](docs/rules.md)
- [Symbols and structural editing](docs/symbols.md)
- [Sandbox](docs/sandbox.md)
- [System prompt](docs/prompt.md)
- [Sessions and compaction](docs/sessions.md)
- [Terminal and keys](docs/terminal.md)

Clients and embedding:

- [Protocol](docs/protocol.md)
- [Router and A2A](docs/serve.md)
- [Embed in Rust](docs/sdk.md)
- [Build a client](docs/integrate.md)
- [Example plugins](docs/examples.md)
- [Philosophy: one door](docs/philosophy.md)

Dalgona product:

- [Batteries](dalgona/docs/batteries.md)
- [Settings that differ](dalgona/docs/config.md)
- [Orchestration](dalgona/docs/orchestration.md)
- [Eval packs](dalgona/docs/eval.md)
- [Dalgona philosophy](dalgona/docs/philosophy.md)
- [Dalgona readme](dalgona/README.md)

Project contracts:

- [Concepts](CONCEPTS.md)
- [Design](DESIGN.md)
- [Branding and names](BRANDING.md)
- [Changelog](CHANGELOG.md)
- [License](LICENSE.md)

## Development

No `CONTRIBUTING.md` is checked in.

Work from the repository root. It holds one Cargo workspace and one `Cargo.lock`. Use the latest stable Rust toolchain.

Mirror the dal CI lane before review (`.github/workflows/dal.yml`):

```sh
cargo fmt --check
cargo clippy --workspace --exclude dalgona --exclude dalgona-batteries --exclude dalgona-gates --all-targets --all-features --locked -- -D warnings
cargo test --workspace --exclude dalgona --exclude dalgona-batteries --exclude dalgona-gates --all-features --locked
cargo deny --locked check
```

The dal lane excludes the dalgona packages; the dalgona lane (`.github/workflows/dalgona.yml`) covers `dalgona`, `dalgona-batteries`, and `dalgona-gates` the same way, so match the scope to your change. `AGENTS.md` lists the merge gates. The timing-lane stress budgets (`DAL_TIMING_BUDGETS`) run only on schedule or manual dispatch, not on every push.

Regenerate the dal manual after doc changes with the `docs-gen` command recorded in `AGENTS.md`. Keep Linux, macOS, and Windows behavior in mind. Do not publish a crate, create or push a tag, create a release, or run a release workflow without explicit approval.

Report issues and propose changes as GitHub issues and pull requests against `gosuda/dalgona`.

## License

Dal and dalgona are source-available under the Sustainable Use License, Version 1.0, with licensor `metaphorics`. Read the full terms in [LICENSE.md](LICENSE.md).

This repository also includes Starlark translations of examples from pi (https://github.com/earendil-works/pi), Copyright (c) 2025 Mario Zechner, used under the MIT License; the license text is in [docs/examples.md](docs/examples.md).
