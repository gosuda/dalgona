# Find anything

Search paths are workspace-relative. Absolute paths and `..` are refused. Search skips `.git`, paths that `.gitignore` or `.git/info/exclude` exclude, binary files, and files over 16 MiB, and it does not follow symbolic links. A file missing from results may be one of those.

## find

Use `search` with mode `find` when you know a path pattern or need to discover files by name. It is the first choice for locating a file without reading its contents. A glob without `/` matches the file name at any depth. Results are sorted and capped by `limit` (default 200).

Example: `search(mode = "find", pattern = "src/**/*.rs")`.

## grep

Use `search` with mode `grep` when you know text, a literal, or a regular expression that should occur in file contents. Set `path` when the search should stay inside one directory or file. Set `glob` to search only files whose path matches it, `literal` to treat `pattern` as plain text, and `ignore_case` for case-insensitive matching. Results are `path:line:text`, capped by `limit` (default 200).

Example: `search(mode = "grep", pattern = "TODO", path = "src")`.

## symbol

Use `search` with mode `symbol` when you need a definition by name, a qualified name, or a file outline. Symbol mode is available only when `symbol` appears in the mode enum of the offered `search` schema. When it is absent, use `grep` instead.

Example: `search(mode = "symbol", pattern = "Engine::run")`.

- A bare name matches any definition whose qualified name ends with it, so `run` matches `Engine::run` and `Worker::run`. A leading `::` pins the match to the complete qualified name.
- `apply[2]` picks the second definition of identical names in the same file.
- `pattern = "*"` with `path` set to one file lists that file's definitions in source order. Example: `search(mode = "symbol", pattern = "*", path = "src/lib.rs")`.

When exactly one definition matches, the result shows its whole text with a tag. Pass that tag to `patch` when you replace the text.

## monitor

Use `monitor` when a background exec job in this session should wake you on matching output as it streams. Watch one job id, a UUIDv7, with a regex filter; the matching lines arrive as messages without polling. The watch ends when the job ends, and the job's own report still arrives. It is offered only while monitoring is enabled.

Example: `monitor(action = "watch", job = "<job-id>", filter = "panic")`.

## web_search

Use `web_search` when the answer needs current public information that is not in the workspace. It is offered only while the web battery is enabled. Use the configured provider and keep the query narrow: `query` takes 1 to 400 characters, and `count` takes 1 to 20, default 5.

Example: `web_search(query = "tree-sitter Rust parser API")`.

## Deferred tools

Use the deferred-tool search offered in the active tool set. If `tool_search` is offered, search for the capability and inspect the returned names and parameters. Then call the exact tool name.

Example, when `procs` is offered: `tool_search(query = "background jobs")`.

`tool_search` matches its query as one case-insensitive substring against each tool's name and description, so a multi-word query matches only where those words appear together and in that order. Use one distinctive word when unsure. It returns at most 50 matches.

The built-in `tool_search` lists matches without activating them. Calling a listed tool activates it. `tool_search` changes nothing: call a listed tool by name and it activates on that first call. When Anthropic native tool search is offered instead, use that search to discover the tool.

When `procs` is offered, it is the tool for retained background output: `procs(op = "list")` lists this session's jobs, `procs(op = "grep", query = "<regex>")` greps retained output. It stays deferred until the first call names it.
