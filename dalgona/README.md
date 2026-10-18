# dalgona

Dalgona is the opinionated batteries product built on dal through the public extension interface.

It turns on twelve compiled Rust batteries by default (orchestration, history, quality, judged, web, work, MCP, review, ask, skills, ttsr-rules, and procs) and sets defaults for symbol search, `hashline` editing, and the complexity guard while keeping its own configuration, data, and sessions.

---

- [What is this](#what-is-this)
- [Install](#install)
- [Quick start](#quick-start)
- [Batteries](#batteries)
- [Guides and reference](#guides-and-reference)
- [License](#license)

## What is this

Dal is the complete coding agent platform; dalgona is what you build on it. Every dalgona battery registers through the same public extension door that user plugins use, with no private host path.

Binaries are `dalgona` (alias `dg`) alongside `dalgon` (aliases `dal`, `dl`). A plugin that loads in `dalgon` loads in `dalgona`. Dalgona uses its own per-OS configuration and data directories (`~/.config/dalgona/` and `~/.local/share/dalgona/` on POSIX defaults) and never scans dal's data root.

In addition to the shared CLI commands, `dalgona` adds `dalgona eval <pack>` to run Starlark evaluation packs against ephemeral workspaces.

## Install

In a POSIX shell, build from source from the repository root. The install guide in this checkout describes a source route and lists no registry version to install.

You need git, the latest stable Rust toolchain, and a C toolchain. The symbol grammars compile through `cc`.

```sh
git clone https://github.com/gosuda/dalgona
cd dalgona
rustup update stable --no-self-update
cargo build --release --locked -p dalgon -p dalgona
export PATH="$PWD/target/release:$PATH"
```

The binaries land in `target/release/` (`dalgona` and `dg`, plus `dalgon`, `dal`, and `dl`), assuming the default Cargo target directory (`CARGO_TARGET_DIR` and `--target-dir` move it). The `export` line assumes POSIX `sh`; PowerShell needs different syntax to extend `PATH`.

Supported platforms are Linux, macOS, and Windows on x86_64 and aarch64. Unsigned Windows binaries may trigger SmartScreen; verify the checksum where one is published. Plugins need no toolchain.

## Quick start

The outputs below summarize checked-in CLI help and docs.

```sh
dalgona docs dalgona://batteries
# prints the Batteries manual page starting with `# Batteries`
dalgon docs
# prints the dal manual index starting with `# dal://`
dalgon login --help
# prints sign-in usage starting with `Sign in to a provider`
dalgon login status
# exit 1 with no credentials; lists providers as `not configured`
```

Run `dalgona` (or `dg`) for the terminal once a provider is ready. Sign in with `dalgona login`, then pick a model.

## Batteries

Each battery is compiled Rust registered through the public extension API with origin `bundled`. Name a battery in `disabled_batteries` in `dal.toml` to remove it, or set its `[plugin.<name>] enabled = false` where supported.

| battery | registers | page |
|---|---|---|
| ask | tool `ask` | `dalgona://ask` |
| history | compactor `history`, `letter://` images | `dalgona://history` |
| judged | judge-fed hooks, search reranker | `dalgona://judged` |
| mcp | MCP client | `dalgona://mcp` |
| orchestration | goal, monitor, arbiter, and agent controls | `dalgona://orchestration` |
| procs | tool `procs` (deferred) | `dalgona://procs` |
| quality | guard findings, codemod offers, `quality_apply` | `dalgona://quality` |
| review | tool `review`, command `/review` | `dalgona://review` |
| skills | skills `find-anything`, `delegate-with-contracts`, `initializer-and-sprints` | `dalgona://skills` |
| ttsr-rules | nineteen bundled rules in seven sets | `dalgona://rules` |
| web | tools `web_fetch`, `web_search` | `dalgona://web` |
| work | plan and todo tools, `/plan`, `/todos` | `dalgona://work` |

## Guides and reference

Read `dalgona://` for the Dalgona manual and `dal://` for the dal manual. Run `dalgona docs` to list both schemes or `dalgona docs <uri>` to read a page. Every `dalgona://<page>` entry is also mirrored under `dalgona/docs/<page>.md` in the repository checkout.

Product overview and configuration:

- `dalgona://batteries`: bundled battery inventory and `disabled_batteries`
- `dalgona://config`: settings in `dal.toml` that differ from `dal`
- `dalgona://philosophy`: product architecture and composition
- `dalgona://changelog`: version summary (`dalgona/CHANGELOG.md` holds the full product changelog)

Orchestration and workflows:

- `dalgona://orchestration`: background jobs, monitors, wakes, scopes, mailbox, synthetic models, and enforced constants
- `dalgona://agents`: `agents` tool actions (`run`, `wait`, `cancel`, `list`), pools, worktree isolation, and report delivery
- `dalgona://workflows`: saved workflows under `[plugin.orchestration.workflows.<name>]`
- `dalgona://goal`: durable session goals (`/goal`, `/continuation`, `create_goal`, `update_goal`, `get_goal`)

Quality, review, and evaluation:

- `dalgona://quality`: streaming detector lanes, guard receipts, and `quality_apply` codemod offers
- `dalgona://review`: `review` tool and `/review` command over session git diffs
- `dalgona://judged`: judge-fed auto-thinking, search ranking, anchored asks, claim verification, and prompt dedup
- `dalgona://rules`: seven bundled Markdown rule sets and the compiled `detectors` set
- `dalgona://eval`: `dalgona eval <pack>` runner, `evalpack` Starlark builtins, and paired `--against` comparisons

Tools, context, and integrations:

- `dalgona://ask`: typed single-select, multi-select, and free-text questions via the `ask` tool
- `dalgona://history`: `history` image compactor, `letter://` records, and auto-dream consolidation
- `dalgona://mcp`: MCP stdio and streamable-HTTP client declarations, OAuth, and grants
- `dalgona://procs`: deferred `procs` tool (`list` and `grep`) over session background jobs
- `dalgona://web`: `web_fetch` (public URLs only) and `web_search` (Brave and Tavily)
- `dalgona://work` and `dalgona://todo`: read-only `/plan` mode and journal-backed `/todos` (`todo://current`, `todo://terminal`)
- `dalgona://skills`: bundled skills (`find-anything`, `delegate-with-contracts`, `initializer-and-sprints`)
- `dalgona://search` and `dalgona://symbols`: symbol search and structural block editing (`dal://symbols`)

Repository contracts in the checkout root: `README.md`, `CONCEPTS.md`, `DESIGN.md`, `BRANDING.md`, and `LICENSE.md`.

## License

Dal and dalgona are source-available under the Sustainable Use License, Version 1.0, with licensor `metaphorics`. Read the full terms in the root `LICENSE.md`.
