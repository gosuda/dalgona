# Harness sandbox

Run dalgona inside a lightweight outer sandbox (container, microVM, or a cloud sandbox runner). The harness sandbox only keeps commands from writing outside the workspace by accident; it is not a security boundary.

The harness sandbox applies only to the Local runner.

## Modes

`workspace-write` MUST be the default. `off` MUST disable harness confinement. Approval mode `yolo` MUST force `off`.

## workspace-write

A command MAY read any path. Network access MUST stay allowed. A command MAY write only under these roots:

- the session working directory
- `os.TempDir()`
- `os.UserCacheDir()`
- each path in `sandbox_writable_roots`

The runner MUST pass every root through `filepath.EvalSymlinks` before it applies the policy.

## Linux

The shim MUST apply this policy before it execs the command:

```
landlock.V3.BestEffort().RestrictPaths(landlock.RODirs("/"), landlock.RWDirs(roots...), landlock.RWDirs("/dev"))
```

`Available()` MUST report whether the Landlock ABI exists. When the go-landlock v0.11.0 ABI query reports no ABI, the command MUST run without Landlock. That case MUST NOT fail the command.

## macOS

The shim MUST exec `/usr/bin/sandbox-exec -p <profile> -- argv...`. The profile MUST be:

```
(version 1) (allow default) (deny file-write*) (allow file-write* (subpath "<root>")...) (allow file-write* (literal "/dev/null") (literal "/dev/zero") (regex #"^/dev/tty") (regex #"^/dev/fd/"))
```

Each root string MUST escape `"` and `\`.

## No harness sandbox

Windows MUST always run without a harness sandbox. Linux MUST do the same when `Available()` is false. The decider state MUST carry `sandboxed=false`. The status row MUST show `auto (no sandbox)`. The runner MUST NOT raise an error for this fallback. It MUST NOT report an unsandboxed process as confined.

## Remote

A remote box MUST ignore the sandbox mode. The box is the sandbox.

## Failure

The shim MUST apply the policy before it starts the command. A policy error MUST write the error to `out`. The call MUST exit nonzero. The shim MUST still write `exit` and perform the configured `DALGONA_SYNCFS` operation.
