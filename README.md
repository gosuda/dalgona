# dal and dalgona

**A terminal coding agent with one extension door.**

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

Everything extends dal through one public interface. Plugins, other programs, and dalgona's batteries use it, and so does dal's own terminal.

In process, the Host and Agent seam carries one snapshot, one change stream, one command path, one request and answer pair, and one blob read. In plain words, a client reads the session state, follows its updates, sends a command, answers a request from the agent, and fetches stored data. A `Host` owns the live sessions. An `Agent` is a handle to one session.

Across processes, the same operations travel as JSON-RPC over stdio or a local socket. The manuals live behind that door at `dal://` and `dalgona://`. [Philosophy](docs/philosophy.md) explains why dal has one door.

## Install

Build from source. No tagged release exists, so the registry and installer channels have nothing to fetch yet. [Install dal and dalgona](docs/install.md) has the details for each OS.

You need git, the latest stable Rust toolchain, and a C toolchain. The symbol grammars compile through `cc`.

```sh
git clone https://github.com/gosuda/dalgona
cd dalgona
rustup update stable --no-self-update
cargo build --release --locked -p dalgon -p dalgona
export PATH="$PWD/target/release:$PATH"
```

In PowerShell, run the first four commands. Then extend `PATH` with this line instead of `export`:

```powershell
$env:PATH = "$PWD\target\release;$env:PATH"
```

The binaries land in `target/release/` (`dalgon` with `dal` and `dl`, plus `dalgona` with `dg`), assuming the default Cargo target directory (`CARGO_TARGET_DIR` and `--target-dir` move it). Run `dalgon --version` to check the install.

- **Platforms**: Linux, macOS, and Windows on x86_64 and aarch64. Unsigned Windows binaries may trigger SmartScreen; verify the checksum where one is published.
- **Paths**: `$XDG_CONFIG_HOME/dal/` and `$XDG_DATA_HOME/dal/` on every OS; an `XDG_*` value must be an absolute path or it is ignored. Without those variables, the defaults are `~/.config/dal/` and `~/.local/share/dal/` under your home directory. On Windows, the home directory is `HOME`, `USERPROFILE`, or `HOMEDRIVE` with `HOMEPATH`. Dalgona uses `dalgona` in place of `dal`.
- **Plugins need no toolchain**: a plugin is Starlark text loaded at startup.

## Quick start

Sign in, pick a model, and run one prompt. [Quickstart: run your first dal agent turn](docs/quickstart.md) walks through each step.

```sh
dalgon login
dalgon
dalgon -p "Summarize this repository's README"
```

**`dalgon login`** signs in to a provider. **`dalgon`** opens the terminal; type `/model` to pick a model, then send a prompt. **`dalgon -p`** runs one turn and exits; it needs a model. Dal asks before every `patch` and every `exec`.

To verify the install, run these checks. Each comment names the start of the real output or the exit code.

```sh
dalgon --version
# dalgon 0.1.0
dalgon docs
# prints the dal manual index starting with `# dal://`
dalgon docs dal://cli
# prints the command-line page
dalgon login --help
# prints sign-in usage starting with `Sign in to a provider`
dalgon login status
# exit 1 with no credentials; lists providers as `not configured`
```

## Features

- **Complete alone**: chat in the terminal, search and edit with approval, keep journals, and read `dal://` without installing dalgona.
- **Same door**: load any dal plugin in dalgona unchanged. Name a dalgona battery in `disabled_batteries` to leave it out.
- **Starlark plugins**: ship tools, commands, hooks, skills, rules, and prompt text from one `plugin.star` with no compile step. A broken plugin stops startup and names `path:line:col`. `/reload` keeps the previous set on failure.
- **Approval first**: ask before every `patch` and every `exec` by default. Headless runs deny when no person can answer. `eval` runs Starlark only and is not a sandbox.
- **Durable sessions**: continue with `-c`, resume with `-r`, branch with `/fork` and `/clone`, and share with `/export`. Compaction starts at 85 percent of the context window.
- **Terminal and headless**: work inline or fullscreen, print with `-p`, emit JSON with `--json`, or drive ACP, app-server, RPC, router, and A2A clients over one protocol.
- **Batteries on demand**: run dalgona for orchestration, plan and todo, ask, web search and fetch, MCP client, review, judged rules, quality guard, history, skills, ttsr-rules, symbol search, hashline edits, and the deferred `procs` tool over background jobs. `dalgona eval` runs Starlark eval packs.

### Scope limits

Dal keeps its core small:

- **No MCP client** in dal; dalgona ships one, and a plugin can reach HTTP services with `net.fetch`.
- **No plan or todo** in dal; dalgona ships both. **No subagents** by default.
- **No LSP** in either product, **no upload** (`/export` writes a file you share yourself), **no self-rebuild** (`/reload` reloads plugins), **nothing but Starlark** in `eval`, **no second store** besides the journal, and **no browser front end** in the binary.
- **Sandbox off by default**: when on, it guards the home directory and sets no network restriction.
- **A plugin runs with your permissions**: read it before you install it.

## Usage

### Sign in and list models

```sh
dalgon login
dalgon login status
dalgon models
dalgon logout
```

`dalgon models` prints the catalog with built-in prices:

```text
PROVIDER  ID             CONTEXT        PRICE
openai    gpt-5.6        922000 tokens  $4.000/$20.000 per M tokens
openai    gpt-5.6-luna   922000 tokens  $0.200/$1.200 per M tokens
Prices: models.dev, fetched 2026-09-27.
```

**`login`** signs in interactively or with `--api-key` or `--device-auth` where the provider supports it. **`login status`** exits 0 when at least one provider is ready and 1 otherwise, with a sign-in hint and no secret output. **`models`** lists models with built-in prices and sources. **`logout`** removes stored credentials. **Secrets** live in `auth.json` with mode `0600`; the loader refuses symlinks and group or other permission bits.

### Read the manual

```sh
dalgon docs
dalgon docs dal://cli
dalgona docs dalgona://batteries
```

**No URI**: `docs` prints the index. **A page URI**: it prints the page. **Unknown URIs** fail with a miss message on stderr and nothing on stdout.

### Run in the terminal

```sh
dal
dal --screen fullscreen
dalgona
```

**`dal`** starts the inline terminal by default once a provider and model are set. **`--screen fullscreen`** uses the alternate screen and replays the settled transcript on exit. **`dalgona`** starts the batteries product with its own config, data, and sessions. **Type while dal works**: a steer reaches the model at the next safe point, a follow-up waits for the turn to end, and stop ends the turn at once.

### Run one turn without the TUI

```sh
dalgon -p "Summarize the staged diff"
dalgon --json -o reply.md "Summarize the staged diff"
dalgon -c -p "Continue with the next file"
dalgon -r my-session -p "Resume and finish"
```

**Print mode** runs one turn, waits up to 3 seconds for extension status to go quiet, and needs a configured model (without one, dalgon reports the missing model and how to set it). **JSON mode** writes one JSON result or error line on stdout (with the stop reason, not the assistant text) only after every status kind is quiet; pair it with `-o FILE` to save the assistant message. **Common run flags**: `-m` for model, `--thinking`, `--mode`, `-n` for name, `-C` for workspace directory, and `--no-session` to skip saving.

### Control sessions and safety

```sh
dalgon -p --approval ask "Inspect the build script"
dalgon -p --approval edits "Format and update the changelog"
dalgon -p --approval edits --sandbox "Run the test suite"
```

**Run modifiers**: pass `--approval` and `--sandbox` on a TUI or headless invocation (without a prompt, bare `dalgon --approval ...` starts the TUI on a terminal and fails on a pipe). **`ask`** (default) asks before every `patch` and every `exec`. **`edits`** approves edits without asking and still asks for `exec`. **`all`** approves every call and is only for runs already confined by an external sandbox; the built-in `--sandbox` sets no network restriction, so it is not that confinement. **Fail closed**: when no controller can answer, dal denies and says why. **`--sandbox`** runs `exec` children through the platform helper with workspace, temporary, cache, and `sandbox_writable` roots writable. **`dalgon serve`** binds loopback only unless `--public` is given with a token from `dalgon serve token`.

### Manage plugins

```sh
dalgon plugin list
dalgona plugin grant quality
dalgona plugin revoke quality
```

**Data root only**: user plugins never load from the workspace and must be listed in `plugins` in `dal.toml` before `dalgon plugin grant <name>` can grant them. **Bundled batteries**: in `dalgona`, batteries such as `quality` are already registered (`dalgon` alone rejects `dalgon plugin grant quality` as not configured). **`list`** shows configured plugins and persistent grants. **`grant` and `revoke`** change the stored grant; session grants end with their sessions.

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

**`serve`** starts the local router; **`--public`** listens wider and requires a token, and **`--a2a`** enables the agent-to-agent surface. **`rpc`** serves JSON-RPC over stdio or a local socket. **`completion`** prints shell completions for `bash`, `elvish`, `fish`, `powershell`, and `zsh`. **`rules test`** simulates one stream rule against the given text with `--source`, `--tool`, and `--path` selectors.

## Documentation

The same pages live inside the binaries and in the checkout. Use `dalgon docs` for `dal://` and `dalgona docs` for `dalgona://`.

Core use:

- [Quickstart](docs/quickstart.md)
- [Install](docs/install.md)
- [Command line](docs/cli.md)
- [Slash commands](docs/commands.md)
- [Settings in dal.toml](docs/config.md)
- [Providers and models](docs/models.md)
- [Tools, modes, and approval](docs/tools.md)

Extension and session control:

- [Plugins](docs/plugins.md)
- [Port a pi extension](docs/convert-pi.md)
- [Rules](docs/rules.md)
- [Symbols and structural editing](docs/symbols.md)
- [Sandbox](docs/sandbox.md)
- [Complexity guard](docs/guard.md)
- [System prompt](docs/prompt.md)
- [Sessions and compaction](docs/sessions.md)
- [Terminal and keys](docs/terminal.md)

Clients and embedding:

- [Protocol](docs/protocol.md)
- [Router and A2A](docs/serve.md)
- [Embed in Rust](docs/sdk.md)
- [Build a client](docs/integrate.md)
- [Example plugins](docs/examples.md)
- [Philosophy: why dal has one extension door](docs/philosophy.md)

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
- [Product requirements (maintainer spec)](docs/requirements.md)
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
