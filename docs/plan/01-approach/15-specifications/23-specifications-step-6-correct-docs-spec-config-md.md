#### Specifications step 6. Correct `docs/spec/config.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/config.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 21 | This shape resolves a conflict in the original plan. TOML cannot hold "approval = \"auto\"" and `[approval]` in one document. The plan therefore intended one scalar and one table, but used one key for both. `[approval_decider]` preserves both behaviors without renaming the approval mode. `approval_cfg` and `[approval]` are not valid configuration keys. | `approval_cfg` and `[approval]` are not valid configuration keys. | config_test.go:105-106 rejects `approval_cfg` with `unknown key`; config_test.go:345-349 `TestLoad_rejectsLegacyApprovalTable` rejects a `[approval]` table |
| 35 | Only `WriteSetting` and the helpers built on it (`TrustedWorkspaces`, `TrustWorkspace`, `CanonicalWorkspace`) MAY write it. | Only `WriteSetting` and `TrustWorkspace` write this key. | merge.go:646 `return WriteSetting(home, "mcp_trusted_workspaces", append(trusted, clean))` is the only writer that names this key; merge.go:622 `TrustedWorkspaces` only returns the stored list; merge.go:625-635 `CanonicalWorkspace` only computes a path; rpc/se |
| 35 | Every entry MUST be a nonempty workspace path, stored in canonical form (`filepath.Abs` plus `EvalSymlinks`, falling back to `Clean`). | Every entry MUST be a nonempty workspace path. `TrustWorkspace` MUST store the workspace it adds in canonical form (`filepath.Abs` plus `EvalSymlinks`, falling back to `Clean`). | merge.go:637-646 `clean := CanonicalWorkspace(cwd)` canonicalizes the one entry that TrustWorkspace adds; merge.go:622 returns stored entries without any check |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 7 | If a layer cannot be read or fails validation, `Load` MUST return an error. | config.go:277-279 `if layer.err != nil { return Config{}, layer.err }`; config.go:284-289 returns validateProductLayer and merge errors; config.go:292-294 returns validate errors |
| 9 | When no layer sets `edit_style`, every model MUST use the edit style `anchor`. | config.go:261 sets EditStyleSpec{Default: anchor} in defaultConfigWithHome; config_test.go:59 pins requireConfigValue(cfg.EditStyle.EditStyle(any/model), anchor) |
| 23 | When no layer sets `[approval_decider] threshold`, its value MUST be `0.9`. | config.go:263 sets ApprovalDeciderConfig{Threshold: .9}; config_test.go:62 pins requireConfigValue(cfg.ApprovalDecider.Threshold, 0.9) |

Structure:

- Line 21: the design-history paragraph is removed and its two rules are kept.
- Line 7: the Load failure rule is added.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 5 | On Windows, the `dal` host start and the dalgona daemon start MUST reset the protected DACL on the product home. The same reset MUST apply to every file and directory inside the home. The same reset MUST NOT change a symlink or an irregular entry. |
| 27 | Each key MUST merge across layers on its own. A key present in a later layer MUST replace the earlier value, including an empty string. An absent key MUST keep the earlier value. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 2 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/config.md` prints `style findings: 0`, and `just verify` passes.

