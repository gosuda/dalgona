# Discovery

Discovery MUST be off by default. The configuration MUST name each enabled source. The supported names are `codex`, `claude`, and `claude-plugins`. An unknown name MUST fail with `discovery: unknown source "<name>"`.

A `dal` configuration MUST NOT set `discovery`. Loading such a configuration MUST fail with `<file>:<line>: discovery is a dalgona setting`.

The loader MUST merge native configuration first. It MUST then merge `claude`, `codex`, and `claude-plugins` in that order. This order MUST NOT depend on the order of the configured source names. The first MCP server or skill with a name wins. Instructions have no name and MUST all remain in the result. The loader MUST preserve each instruction path and text.

Within one source, home entries MUST come before workspace entries.

## Codex

The `codex` source MUST read these files when they exist. Files under the home directory define home servers. Files under the current working directory define workspace servers and MUST carry the workspace mark:

- `~/.codex/AGENTS.md` and `<cwd>/.codex/AGENTS.md`.
- `~/.codex/config.toml` and `<cwd>/.codex/config.toml`, under `mcp_servers`.
- `~/.codex/skills/*/SKILL.md` and `<cwd>/.codex/skills/*/SKILL.md`.

## Claude

The `claude` source MUST read these files when they exist. Files under the home directory define home servers. Files under the current working directory define workspace servers and MUST carry the workspace mark:

- `~/.claude/CLAUDE.md` and `<cwd>/.claude/CLAUDE.md`.
- `~/.claude.json`, `~/.claude/mcp.json`, `<cwd>/.claude/mcp.json`, and `<cwd>/.mcp.json`, under `mcpServers`.
- `~/.claude/skills/*/SKILL.md` and `<cwd>/.claude/skills/*/SKILL.md`.

The loader MUST read the `claude` MCP files in the order listed.

## Claude plugins

The `claude-plugins` source MUST read `~/.claude/plugins/installed_plugins.json`. The loader MUST skip each registry entry whose `installPath` is empty. The plugin name is the part of its plugin id before the last `@`.

The loader MUST process plugin ids in sorted order. The loader MUST skip a plugin id with no `@`, an empty name, or an empty marketplace.

The loader MUST skip an entry whose `enabled` field is `false`. It MUST skip every entry for a plugin whose value is `false` in `enabledPlugins` in `~/.claude/settings.json`. Other value types in `enabledPlugins` MUST be ignored. A disabled plugin MUST NOT have its manifest, skills, or MCP configuration read.

For each enabled plugin root, the loader MAY read `.claude-plugin/plugin.json`. The `skills` field selects one skill directory or an array of skill directories. Its default is `skills`. The loader MUST skip each non-string entry in a `skills` array. The `mcpServers` field has these forms:

- An object contains the MCP server map directly.
- A string names a JSON file below the plugin root. This file MUST exist.
- An absent or null field selects `<root>/.mcp.json`. This fallback file is optional.

An empty `mcpServers` string MUST select `<root>/.mcp.json`, as an absent field does.

An MCP JSON file MAY contain the server map directly or under `mcpServers`. Every plugin MCP name MUST have the `<plugin>:` prefix. Environment maps MUST become sorted `KEY=value` entries. `${CLAUDE_PLUGIN_ROOT}` and `${OMP_PLUGIN_ROOT}` in commands, arguments, and environment values MUST expand to the plugin root. Manifest paths MUST stay inside the plugin root.

## Skills

A skill file MUST start with `---` on its first line. A later `---` line MUST close the frontmatter. The `name` and `description` fields define the skill. LF and CRLF line endings are valid. A missing or empty name MUST use the containing directory name. A skill directory MAY be a symbolic link to a directory.

Instruction files MUST stay inside their home directory or workspace root. The loader MUST resolve each candidate and MUST ignore a file that resolves outside its root. A link inside the root MAY load.

The loader MUST ignore missing optional files and directories. Malformed TOML, JSON, or skill frontmatter MUST return an error that names the file. The loader MUST NOT silently drop a malformed enabled source.
