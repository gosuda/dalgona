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

## web_search

Use `web_search` when the answer needs current public information that is not in the workspace. Use the configured provider and keep the query narrow.

Example: `web_search(query = "tree-sitter Rust parser API")`.

## Deferred tools

Use `tool_search` when the required tool is not in the active tool set. Search for the capability, inspect the returned names and parameters, and then call the exact tool name.

Example: `tool_search(query = "background output")`.

tool_search changes nothing; call a listed tool by name and it activates on that first call.
