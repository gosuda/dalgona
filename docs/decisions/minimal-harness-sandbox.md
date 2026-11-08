# Minimal harness sandbox

## Invariant

The harness sandbox MUST prevent accidental writes outside the allowed roots. It MUST NOT claim to isolate hostile code. Users SHOULD run dalgona in an outer container, microVM, or cloud sandbox.

`workspace-write` MUST be the default local policy. Reads and network access MUST remain allowed. `off` MUST disable confinement. Approval mode `yolo` MUST force `off`. Windows and Linux without Landlock MUST report that no harness sandbox is active.

## Scope

This decision applies to local execution in `internal/runner`. It does not add a second sandbox inside a remote box. [sandbox.md](../spec/sandbox.md) defines roots, platform behavior, and failure handling.

## Enforcement

Runner sandbox tests in `internal/runner` check policy construction and platform behavior. The shim MUST write a nonzero exit record when policy application fails. Review MUST reject any status or documentation that calls the harness policy a security boundary. `just test` runs the Go tests; platform-specific behavior still requires the matching host.
