# Minimal harness sandbox

## Invariant

The harness sandbox MUST prevent accidental writes outside the allowed roots. It MUST NOT claim to isolate hostile code. [sandbox.md](../spec/sandbox.md) describes the outer sandbox.

`workspace-write` MUST be the default local policy. Reads and network access MUST remain allowed. `off` MUST disable confinement. Approval mode `yolo` MUST force `off`. Windows and Linux without Landlock MUST report that no harness sandbox is active.

On macOS, the harness sandbox is active when `/usr/bin/sandbox-exec` exists, is not a directory, and has an execute bit set.

## Scope

This decision applies to local execution in `internal/runner`. It does not add a second sandbox inside a remote box. [sandbox.md](../spec/sandbox.md) defines roots, platform behavior, and failure handling.

## Enforcement

Runner sandbox tests in `internal/runner` check policy construction and platform behavior. The shim MUST write a nonzero exit record when policy application fails. Review MUST reject any status or documentation that calls the harness policy a security boundary. `just test` runs the Go tests. Platform-specific behavior still requires the matching host.

## Rejected alternatives

- A container or virtual machine around every local command. It lost because this sandbox exists to stop accidental writes. Isolating hostile code is outside this sandbox, as the [decision index](../README.md) says. [sandbox.md](../spec/sandbox.md) defines the write confinement.
- Blocking network access by default. It lost because installing packages and fetching sources are normal agent work, and the policy guards writes.
