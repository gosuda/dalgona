# dalgona

dalgona is dal with batteries, and every built-in battery is enabled by default through dal's same public extension door. The built-in batteries are orchestration, history, quality, judged, web, work, MCP, review, ask, skills, and ttsr-rules. By default, Dalgona enables symbol search, AST edit, the `hashline` edit style, and the complexity guard; it also provides plan and todo, rich ask, web search and fetch, an MCP client, and a review loop. It keeps its own configuration, data, and sessions. Dalgona builds only against released dal crates. dal and dalgona are distributed under the Sustainable Use License; the licensor is `metaphorics`.

## Install

```sh
cargo install dal
cargo install dalgona
cargo binstall dal
cargo binstall dalgona
curl -fsSL https://github.com/gosuda/dalgona/releases/latest/download/install.sh | bash
```

```powershell
irm https://github.com/gosuda/dalgona/releases/latest/download/install.ps1 | iex
```

Linux, macOS, and Windows on x86_64 and aarch64. Binaries are `dalgon`, `dal`, `dl`, `dalgona`, and `dg`. Config and data roots follow the operating system and product name. Plugins need no toolchain. Unsigned Windows binaries may trigger SmartScreen; verify the release checksum.

## Batteries

Each battery is compiled Rust registered through the public extension API with origin `bundled`. Name a battery in `disabled_batteries` to remove it.

| battery | registers | page |
|---|---|---|
| ask | tool `ask` | dalgona://ask |
| history | compactor `history`, `letter://` images | dalgona://history |
| judged | judge-fed hooks, search reranker | dalgona://judged |
| mcp | MCP client | dalgona://mcp |
| orchestration | goal, monitor, arbiter, and agent controls | dalgona://orchestration |
| quality | guard findings, codemod offers, `quality_apply` | dalgona://quality |
| review | tool `review`, command `/review` | dalgona://review |
| skills | skills `find-anything`, `delegate-with-contracts`, `initializer-and-sprints` | dalgona://skills |
| ttsr-rules | nineteen bundled rules in seven sets | dalgona://rules |
| web | tools `web_fetch`, `web_search` | dalgona://web |
| work | plan and todo tools, `/plan`, `/todos` | dalgona://work |

## Documentation

Read `dalgona://` for the Dalgona manual and `dal://` for the dal manual. Use `dalgon docs` to list and read pages.

## License

Distributed under the Sustainable Use License; the licensor is `metaphorics`. See the root `LICENSE.md`.
