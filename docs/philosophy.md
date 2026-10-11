# Philosophy: one door

# dal

dal is a small coding agent with one door. Everything that extends it comes in through that door: plugins, other programs, every way in, and dalgona's batteries. The command is `dal`; `dal` and `dl` mean the same.

## Why dalgon

No private inside; its own batteries register through the plugin interface. dal alone is a complete agent; dalgona adds opinions; both load the same plugins.

## One door

In process, the Rust interface of the Host and Agent seam carries five operations over one snapshot, one change stream, one command path, one request and answer pair, one blob read; the TUI uses these and nothing else. Across processes, a JSON-RPC protocol carries the same operations, served by `dalgon rpc` over stdio or a local socket, with a JSON Schema for every message; `docs/read` serves both manuals.

## Plugins in Starlark

A plugin is a directory with one `plugin.star` in the data root. A plugin publishes one `plugin` value that registers tools, commands, hooks, skills, rules, and prompt text, and it declares the operations each tool and hook may request; every effect still needs your approval. Nine lifecycle events exist, each with a declared result type; a Rust extension sees a tenth. Questions travel as data to whichever way in you use. Load is free of compile steps. A broken plugin stops startup and names `path:line:col`. `/reload` re-evaluates and prints counts (for example `Reloaded 2 plugins: 3 tools. New turns use them. A running turn keeps the plugins it started with.`); on failure it prints `reload failed: <rendered error>` and `the previous plugin set stays live`. A plugin that loads in dalgon loads in dalgona. A plugin runs with your permissions, so read it before you install it.

## Models

Four API families exist. Run `dalgon login`, then pick a model. `/model` and `/thinking` change them in a session.

## History you can trust

Each session is a journal; a record counts as saved only after it is synced to disk. A session is a tree; `-c` continues, `-r` resumes; `/export` writes a file you can share yourself.

## Context you control

The system prompt is built from parts you control (`AGENTS.md` files, `SYSTEM.md`, skills, rules, plugin sections); `dal://prompt` lists them. Compaction starts by default at 85 percent of the context window. Long, stable text can go out as an image and the exact text stays readable at `letter://<id>`.

## Tools and approval

Normal mode gives the model `read`, `search`, `patch`, and `exec`; eval-first adds `eval`; eval-only leaves only `eval` visible. A call you deny outside `eval` is denied inside it, and `eval` is not a sandbox. `patch` speaks several edit styles, one setting picks the style, and the style follows the model. By default dal asks before every `patch` and every `exec`. `--approval ask|edits|all` carries the ladder. When no person can answer, dal denies and says why. An optional sandbox, off by default, guards the home directory.

## Steer or follow up

You can type while dal works. A steer reaches the model at the next safe point. A follow-up waits for the turn to end. Stop ends the turn at once.

## Many ways in, one protocol

| Way in | Start |
|---|---|
| Terminal (inline) | `dal` |
| Terminal (fullscreen) | `dal` with fullscreen screen |
| Print | `dalgon` print flags |
| JSON | `dalgon` JSON flags |
| ACP | editor ACP adapter |
| RPC | `dalgon rpc` |
| Router | `dalgon serve` |
| A2A | `dalgon serve --a2a` |

`dalgon serve` listens on your machine only unless you add `--public`, which requires a token. No desktop, web, or mobile app ships; any such app is a client of the protocol, not a fork.

## Built for large runs

One session can run more than 500 subagents or more than 200 shell commands at once and still answer your keys. The gate measures it: RSS under 256 MiB, open files under 2048, keypress echo P99 under 24 ms, against a scripted model. These are test gates, not promises about your machine.

## Text in every language

dal measures text the way your terminal draws it; the cursor, wrapping, and cuts never split a character.

## The manual is inside

dal documents itself at `dal://`; dalgona adds `dalgona://`; the model reads pages with `read`; you read them with `dalgon docs`.

## dalgona

dalgona is dal with batteries, and every built-in battery is enabled by default through dal's same public extension door. The built-in batteries are orchestration, history, quality, judged, web, work, MCP, review, ask, skills, and ttsr-rules. By default, Dalgona enables symbol search, AST edit, the `hashline` edit style, and the complexity guard; it also provides plan and todo, rich ask, web search and fetch, an MCP client, and a review loop. It keeps its own configuration, data, and sessions. Dalgona builds only against released dal crates. dal and dalgona are distributed under the Sustainable Use License; the licensor is `metaphorics`.

## What we did not build

No MCP client in dal (dalgona ships one; a plugin can reach HTTP services with the `net.fetch` operation). No plugins from the workspace. No sandbox by default and no network restriction. No plan or todo in dal (dalgona ships both). No subagents in dal by default (the `agents` operations are one door away). No LSP, ever, in either product (search plus AST edit is the surface). No upload (`/share` writes a file). No self-rebuild (`/reload` reloads plugins). Nothing but Starlark in `eval`. No second store (the journal is the only store). No browser front end in the binary.

<!-- grounding (maintainers, not shipped) -->
