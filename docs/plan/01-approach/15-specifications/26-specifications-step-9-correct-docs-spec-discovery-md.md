#### Specifications step 9. Correct `docs/spec/discovery.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/discovery.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 25 | Each registry entry MUST have a non-empty `installPath`. | The loader MUST skip each registry entry whose `installPath` is empty. | internal/discovery/plugins.go:101 `if entry.InstallPath == "" \|\| (entry.Enabled != nil && !*entry.Enabled) {` then `continue` |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 3 | A `dal` configuration MUST NOT set `discovery`. Loading such a configuration MUST fail with `<file>:<line>: discovery is a dalgona setting`. | internal/config/merge.go:530 `var dalgonaOnlyKeys = map[string]bool{... "discovery": true ...}`; internal/config/merge.go:540 `if dalgonaOnlyKeys[root] && ...`; internal/config/merge.go:548 `return fmt.Errorf("%s:%d: %s  |
| 5 | Within one source, home entries MUST come before workspace entries. | internal/discovery/discovery.go:131 `for _, root := range roots {` (codex MCP files, home root first in the roots list built before it); internal/discovery/discovery.go:153-156 (claude MCP paths, home entries first); int |
| 21 | The loader MUST read the `claude` MCP files in the order listed. | internal/discovery/discovery.go:153-156: `~/.claude.json`, `~/.claude/mcp.json`, `<cwd>/.claude/mcp.json`, `<cwd>/.mcp.json` in that order. Add as a new paragraph after the bullet list. |
| 25 | The loader MUST process plugin ids in sorted order. The loader MUST skip a plugin id with no `@`, an empty name, or an empty marketplace. | internal/discovery/plugins.go:78 `for _, id := range slices.Sorted(maps.Keys(registry)) {`; internal/discovery/plugins.go:79-80 `name, ok := pluginName(id)` then `if !ok \|\| pluginDisabled(id, settings) {`; internal/disco |
| 29 | The loader MUST skip each non-string entry in a `skills` array. | internal/discovery/plugins.go:185-186 `if len(value) == 0 \|\| value[0] != '"' {` then `continue`; function pluginSkillPathArray at plugins.go:177 |
| 32 | An empty `mcpServers` string MUST select `<root>/.mcp.json`, as an absent field does. | internal/discovery/plugins.go:254-255 `if configured == "" {` then `return optionalPluginServerFile(filepath.Join(root, ".mcp.json"))`; plugins.go:238-241 selects the same file for an absent or null field. Appended to th |

Use one word for one thing.

- Keep `plugin id`. Replace registry key at lines 25.
- Keep `plugin id`. Replace id (in "whose id is `false`") at lines 27.

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 0 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/discovery.md` prints `style findings: 0`, and `just verify` passes.

