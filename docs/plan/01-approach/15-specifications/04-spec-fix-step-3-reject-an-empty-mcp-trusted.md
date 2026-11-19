#### Spec-fix step 3. Reject an empty `mcp_trusted_workspaces` entry at load
Anchor: Tony Hoare, Richard Feynman

The text is right and the code breaks it. `docs/spec/config.md` line 35 says: "Every entry MUST be a nonempty workspace path." Keep that sentence.

- Where: `internal/config/config.go:Load`.
- Change: In the layer loop of Load (config.go:284-289), after validateProductLayer succeeds and before cfg.merge runs, return an error when layer.doc.MCPTrustedWorkspaces contains an empty string. The error text is `settings.toml: mcp_trusted_workspaces entry must be a nonempty path`, with the base name of layer.path in place of settings.toml. Side: code, because the trust list is a security list and only the loader sees hand-edited files, and rpc/serve_misc.go:163 can write the key through WriteSetting.
- Failing-first test: `TestLoad_rejectsEmptyTrustedWorkspace` in `internal/config/config_test.go`. Input: config.toml empty; settings.toml body mcp_trusted_workspaces = [""] Expected: Load returns an error that contains `settings.toml: mcp_trusted_workspaces entry must be a nonempty path`; today Load returns no error, so the test fails first The expected value comes from: docs/spec/config.md:35 `Every entry MUST be a nonempty workspace path.` gives the rule; the error text is chosen by this plan; the requireConfigError pattern is config_test.go:103-106
- Evidence at commit `3c4b6f94`: merge.go:622 returns `stored.doc.MCPTrustedWorkspaces` with no check; merge.go:530 lists the key as dalgona-only and has no entry check; grep of internal and cmd for mcp_trusted_workspaces and MCPTrustedWorkspaces finds no validator
- Proof: the test fails before the change and passes after it, and `just verify` passes.

