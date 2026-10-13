# Find anything

## find

Use `search` with mode `find` when you know a path pattern or need to discover files by name. It is the first choice for locating a file without reading its contents.

Example: `search(mode = "find", pattern = "src/**/*.rs")`.

## grep

Use `search` with mode `grep` when you know text, a literal, or a regular expression that should occur in file contents. Set `path` when the search should stay inside one directory.

Example: `search(mode = "grep", pattern = "TODO", path = "src")`.

## symbol

Use `search` with mode `symbol` when you need a definition by name, a qualified name, or a file outline. Use the result's definition tag before an AST edit.

Example: `search(mode = "symbol", pattern = "Engine::run")`.

## procs

Use `procs` when the answer is in the retained output of a background job in this session. List jobs first when you need an id; grep job output when you know the text.

Example: `procs(op = "grep", query = "panic")`.

## monitor

Use `monitor` when a background exec job in this session should wake you on matching output as it streams. Watch one job id with a regex filter; the matching lines arrive as messages without polling.

Example: `monitor(action = "watch", job = "<job-id>", filter = "panic")`.

## web_search

Use `web_search` when the answer needs current public information that is not in the workspace. Use the configured provider and keep the query narrow.

Example: `web_search(query = "tree-sitter Rust parser API")`.

## Deferred tools

Use the deferred-tool search offered in the active tool set. If `tool_search` is offered, search for the capability and inspect the returned names and parameters. Then call the exact tool name.

Example: `tool_search(query = "background output")`.

The built-in `tool_search` lists matches without activating them. Calling a listed tool activates it. When Anthropic native tool search is offered instead, use that search to discover the tool.
