#### Spec-fix step 6. Resolve symbolic links in manifest paths before the containment check
Anchor: Tony Hoare, Richard Feynman

The text is right and the code breaks it. `docs/spec/discovery.md` line 35 says: "Manifest paths MUST stay inside the plugin root." Keep that sentence.

- Where: `internal/discovery/plugins.go:pluginPath`.
- Change: Keep the lexical check at plugins.go:301-307. When the target exists, compare filepath.EvalSymlinks(target) with filepath.EvalSymlinks(base) using filepath.Rel, and reject a result that starts with .. with the existing text `plugin path %q escapes plugin root %s`. If EvalSymlinks returns fs.ErrNotExist, return the lexical target unchanged, so the existing read errors stay the same. pluginPath serves skill paths (plugins.go:149) and the MCP string path (plugins.go:257), so one change covers both.
- Failing-first test: `TestPluginMCPStringSymlinkOutsideRootIsRejected` in `internal/discovery/discovery_test.go`. Input: installed plugin root <tmp>/plugins/p with .claude-plugin/plugin.json = {"mcpServers":"link.json"}; <root>/link.json is a symbolic link to <tmp>/outside.json, which holds one valid server map; registry and home set up as the plugin tests at discovery_test.go:160-175 do Expected: discovery returns an error whose text contains `plugin path "link.json" escapes plugin root` The expected value comes from: The message format is the existing one at internal/discovery/plugins.go:306, which the lexical input ../outside.json already produces. Spec line 35 is the rule.
- Evidence at commit `3c4b6f94`: internal/discovery/plugins.go:289 `base, err := filepath.Abs(root)`, plugins.go:294 `if !filepath.IsAbs(target) {`, plugins.go:301 `relative, err := filepath.Rel(base, target)`; plugins.go:306 `return "", fmt.Errorf("plugin path %q escapes plugin root %s", configured, root)`; grep for EvalSymlinks in internal/discovery: no match
- Proof: the test fails before the change and passes after it, and `just verify` passes.

