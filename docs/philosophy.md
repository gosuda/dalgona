# Philosophy: why dal has one extension door

dal is a small coding agent with one door. Everything that extends it comes in through that door: plugins, other programs, every way in, and dalgona's batteries. The primary binary is `dalgon`, with `dal` and `dl` as aliases.

## Why dal

dal has no private inside. Its own batteries register through the plugin interface. dal alone is a complete agent. dalgona adds opinions. Both load the same plugins. The [requirements](requirements.md) page lists what dal and dalgona must meet.

## One door

The door is one public interface that plugins, programs, and clients share. In process, the Rust interface of the Host and Agent seam carries five operations. They are one snapshot, one change stream, one command path, one request and answer pair, and one blob read. In plain words, a client reads the session state, follows its updates, sends a command, answers a request from the agent, and fetches stored data. A `Host` owns the live sessions. An `Agent` is a handle to one session. The TUI uses these and nothing else. Across processes, a JSON-RPC protocol carries the same operations. `dalgon rpc` serves it over stdio or a local socket. Every message has a JSON Schema. `docs/read` serves both manuals.

## Plugins in Starlark

A plugin is a directory with one `plugin.star` in the data root. A plugin publishes one `plugin` value. The value registers tools, commands, hooks, skills, rules, and prompt text. It also declares the operations each tool and hook may request. Every effect still needs your approval. Nine lifecycle events exist. Each has a declared result type. A Rust extension sees a tenth. Questions travel as data to whichever way in you use. Load has no compile step. A broken plugin stops startup and names `path:line:col`. `/reload` evaluates the plugins again and prints counts (for example `Reloaded 2 plugins: 3 tools. New turns use them. A running turn keeps the plugins it started with.`). On failure, it prints `reload failed: <rendered error>` and `the previous plugin set stays live`. A plugin that loads in dalgon loads in dalgona. A plugin runs with your permissions, so read it before you install it. To bring over a pi extension, follow [Port a pi extension to a dal plugin](convert-pi.md).

## Models

Four API families exist. Run `dalgon login`. Then pick a model. `/model` changes the model in a session. `/thinking` changes the thinking level in a session. See [Providers, models, and thinking levels](models.md).

## History you can trust

Each session is a journal. A record counts as saved only after dal syncs it to disk. A session is a tree. `-c` continues a session. `-r` resumes a session. `/export` writes a file you can share yourself. See [Sessions, history, and compaction](sessions.md).

## Context you control

dal builds the system prompt from parts you control (`AGENTS.md` files, `SYSTEM.md`, skills, rules, plugin sections). `dal://prompt` lists them. See [The system prompt](prompt.md). Compaction starts by default at 85 percent of the context window. Long, stable text can go out as an image. The exact text stays readable at `letter://<id>`.

## Tools and approval

Every mode lists `read`, `search`, `patch`, `exec`, and `eval`. The eval-first and eval-only modes also reach plugin tools and prompt sections marked `eval_only`. [Tools, modes, and approval](tools.md) explains each mode. A call you deny outside `eval` stays denied inside it. `eval` is not a sandbox. `patch` supports several edit styles. One setting picks the style, and the style follows the model. By default dal asks before every `patch` and every `exec`. `--approval ask|edits|all` sets the approval ladder. When no person can answer, dal denies the call and says why. An optional sandbox, off by default, guards the home directory. See [Sandbox](sandbox.md).

## Steer or follow up

You can type while dal works. A steer reaches the model at the next safe point. A follow-up waits for the turn to end. Stop ends the turn at once. See [Terminal interface](terminal.md) for the keys.

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

`dalgon serve` listens on your machine only unless you add `--public`, which requires a token. See [Router and A2A](serve.md). No desktop, web, or mobile app ships. Any such app is a client of the protocol, not a fork. See [Build a client on the dal protocol](integrate.md).

## Built for large runs

The stress gate exercises 501 child sessions and 201 shell jobs with scripted models. On the idle-machine timing lane (`DAL_TIMING_BUDGETS`), it requires RSS below 256 MiB, fewer than 4096 open handles, and cancellation P99 below 250 ms. These are gate limits, not promises about your machine.

## Text in every language

dal measures text the way your terminal draws it. The cursor, wrapping, and cuts never split a character.

## The manual is inside

dal documents itself at `dal://`. dalgona adds `dalgona://`. The model reads pages with `read`. You read them with `dalgon docs`.

## dalgona

dalgona is dal with batteries. dalgona enables every built-in battery by default through dal's same public extension door. The built-in batteries are orchestration, history, quality, judged, web, work, MCP, review, ask, skills, and ttsr-rules. By default, dalgona enables symbol search, AST edit, the `hashline` edit style, and the complexity guard. It also provides plan and todo, rich ask, web search and fetch, an MCP client, and a review loop. It keeps its own configuration, data, and sessions. This repository uses one root Cargo workspace and one root `Cargo.lock` to resolve dalgona's dal dependencies locally. Releases publish dal and dalgona separately. dal and dalgona are distributed under the Sustainable Use License. The licensor is `metaphorics`.

## What we did not build

dal has no MCP client. dalgona ships one. A plugin can reach HTTP services with the `net.fetch` operation. dal does not load plugins from the workspace. dal has no sandbox by default and no network restriction. dal has no plan or todo. dalgona ships both. dal has no subagents by default. The `agents` operations are one door away. Neither dal nor dalgona has LSP. Search plus AST edit is the surface. dal never uploads. `/export` writes a file you can share yourself. dal has no self-rebuild. `/reload` reloads plugins. `eval` accepts nothing but Starlark. dal has no second store. The journal is the only store. The binary has no browser front end.

## Next

- [Quickstart](quickstart.md): sign in and run your first turn.
- [Plugins](plugins.md): add a tool or command through the door.
- [Protocol](protocol.md): build a client on the same operations.
