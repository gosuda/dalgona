# Batteries

Every battery is compiled Rust registered through the public extension API with origin `bundled`. Name a battery in `disabled_batteries` and it registers nothing. A battery whose `[plugin.<name>]` table sets `enabled = false` also registers nothing. The `dalgona://` manual is a separate built-in extension, not a battery.

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
