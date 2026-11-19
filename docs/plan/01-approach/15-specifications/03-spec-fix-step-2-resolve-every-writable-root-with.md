#### Spec-fix step 2. Resolve every writable root with its longest existing prefix
Anchor: Tony Hoare, Richard Feynman

The text is right and the code breaks it. `docs/spec/approvals.md` line 34 says: "Each root and path MUST pass through `filepath.EvalSymlinks` for its longest existing prefix." Keep that sentence.

- Where: `internal/agent/host.go:writableRoots`, `internal/agent/gate.go:resolveExisting`.
- Change: In `writableRoots`, replace the `filepath.EvalSymlinks(root)` call with `resolveExisting(root)` for every root. The thread working directory already uses `resolveExisting` in `outsideRoots`. A nonexistent root then keeps its longest existing prefix resolved and its missing suffix appended. No error text changes.
- Failing-first test: `TestWritableRootsResolveLongestExistingPrefix` in `internal/agent/host_roots_test.go (new)`. Input: A temp directory with `real/` and a symlink `link` to `real`. `cfg.SandboxWritableRoots = []string{link + "/new"}`, where `new` does not exist. Call `writableRoots(cfg)`. Skip the test when `os.Symlink` fails. Expected: The result contains `filepath.Join(realResolved, "new")`, where `realResolved` is `filepath.EvalSymlinks(real)` computed in the test. The expected value comes from: docs/spec/approvals.md:34 `Each root and path MUST pass through filepath.EvalSymlinks for its longest existing prefix.` The expected value uses the standard library in the test, not the code under test. Current code returns `link/new` unresolved, so the test fails first.
- Evidence at commit `3c4b6f94`: internal/agent/host.go:93-95 `if real, err := filepath.EvalSymlinks(root); err == nil { root = real }`, inside writableRoots (host.go:82-97); internal/agent/gate.go:406-412 `resolveExisting` returns the longest existing prefix resolved, and gate.go outsideRoots already applies it to paths and the thread cwd.
- Proof: the test fails before the change and passes after it, and `just verify` passes.

