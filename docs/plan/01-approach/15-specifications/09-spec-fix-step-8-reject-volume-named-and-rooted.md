#### Spec-fix step 8. Reject volume-named and rooted paths on every OS
Anchor: Tony Hoare, Richard Feynman

The text is right and the code breaks it. `docs/spec/edit-styles.md` line 19 says: "Rooted paths and paths with a volume name MUST fail the same way as absolute paths." Keep that sentence.

- Where: `internal/tool/patch/stage.go:resolvePatchPath`.
- Change: Replace the filepath.IsAbs(raw) test at stage.go:184 with a test that gives the same result on every OS: reject a path that starts with / or a backslash, and reject a path whose first byte is an ASCII letter and whose second byte is a colon. Keep the error text at stage.go:185.
- Failing-first test: `TestResolvePatchPathRejectsVolumeName` in `internal/tool/patch/resolve_path_test.go (new file)`. Input: cwd is t.TempDir(); raw is C:x Expected: the error text equals the error for raw /x with the same cwd, which is `patch: C:x is outside the workspace <cwd>` The expected value comes from: The expected text is the absolute-path error at stage.go:185, and spec line 19 says a volume-named path must fail the same way. On Linux the test fails before the change, because C:x resolves as a relative name.
- Evidence at commit `3c4b6f94`: internal/tool/patch/stage.go:184 `if filepath.IsAbs(raw) || strings.IndexByte(raw, 0) >= 0 {`; stage.go:185 `return "", "", fmt.Errorf("patch: %s is outside the workspace %s", raw, cwd)`; grep for VolumeName in internal/tool/patch: no match
- Proof: the test fails before the change and passes after it, and `just verify` passes.

