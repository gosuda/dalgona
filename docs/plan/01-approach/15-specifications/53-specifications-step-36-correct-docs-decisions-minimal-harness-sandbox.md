#### Specifications step 36. Correct `docs/decisions/minimal-harness-sandbox.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/decisions/minimal-harness-sandbox.md` in one commit. Line numbers are from commit `3c4b6f94`.

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 7 | On macOS, the harness sandbox is active when `/usr/bin/sandbox-exec` exists, is not a directory, and has an execute bit set. | internal/runner/sandbox_darwin.go:15 `const sandboxExec = "/usr/bin/sandbox-exec"`; sandbox_darwin.go:17-20 `func Available() bool { info, err := os.Stat(sandboxExec); return err == nil && !info.IsDir() && info.Mode()&0o |

Structure:

- Default policy: the default config literal in internal/config/config.go (grep this run, around lines 259-262) sets Approval: ApprovalAuto and Sandbox: SandboxWorkspaceWrite; the constants are at config.go:106-107.
- Per-call yolo off: gate.go:93-96 and tools.go:385-386 (Spec-fix step 14).
- Off when unavailable: effective_sandbox.go:15-18 returns SandboxOff when the harness sandbox is not available; effective_sandbox.go:21-24 SandboxAvailable reports runner.Available(); sandbox_linux.go:17-20 Available is false at Landlock ABI 0; sandbox_other.go:10 returns false on every platform that is neither Linux nor Darwin, which includes Windows.
- Confinement and reads: sandbox_linux.go:57 RestrictPaths(RODirs('/'), RWDirs(roots...), RWDirs('/dev')) keeps reads allowed and limits writes to the roots.
- Remote boxes: internal/runner/remote_sandbox_linux_test.go:10-11 TestRemote_ignoresHarnessSandbox pins the no-second-sandbox rule.
- Shim failure: shim.go:31-33 returns 1 when the nested sandboxed exec fails; shim.go:213 writes Exit{Code: 1} on a failure path through finishShim. The parent records the child's nonzero code. shim.go:77-79 writes that code to the `exit` file, and local.go:326 reads the file into `Output.Exit` (local.go:322).

Do not rewrite the rest of this file. Proof: `just verify` passes.

