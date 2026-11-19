#### Specifications step 38. Correct `docs/decisions/secret-masking.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/decisions/secret-masking.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 7 | Unknown placeholders MUST remain unchanged. | Text that does not match a secret placeholder format MUST remain unchanged, and a well-formed placeholder that cannot be restored MUST return an error. | internal/auth/mask_strict_scope_test.go:18-21 `UnmaskStrict(unknown)` returns the input unchanged with a nil error; mask_strict_scope_test.go:35-38 each listed token must fail with 'cannot be restored'; internal/auth/mask.go:105-106 `if token, ok := unresolved |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 7 | Recovery restores pre-edit file images and snapshot blobs through the strict unmask step. | internal/agent/recover.go:330 `raw, err := w.t.host.unmaskStrict(string(data))` inside preImage; the non-test callers are recover.go:329 (preImage) and thread.go:334 (snapshot restore on open). tools.go:333 masks file.Pre with MaskWithError befo |

Structure:

- Boundary at commit `3c4b6f94`: exec.go:48-52 and patch.go:46-50 call UnmaskStrict; tools.go:333-335, 514-516 and 714-716 call MaskWithError before journal or model use.
- Persist before return: mask.go:71 calls syncPatternSecrets before mask.go:80 applyReplacements; a persistence failure returns maskPersistenceFailure (mask.go:74); mask_with_error_test.go:34-37 expects MaskWithError to return an error after the pattern home is broken.
- Short values: mask.go:337-339 and :357-359 skip pattern values shorter than 8 bytes.
- applyReplacements copies text that no item matches unchanged (mask.go:568 and 576). It returns the input when no item matches (mask.go:573-575).

Do not rewrite the rest of this file. Proof: `just verify` passes.

