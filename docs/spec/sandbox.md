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

## Working directory

An `exec` call MUST resolve a relative `cwd` against the thread working directory to an absolute path before the runner starts. When the mode is `workspace-write`, the resolved directory MUST sit inside the writable roots above (the thread working directory counts), otherwise the call MUST fail without starting. In `off` (including an approved escalation) a relative `cwd` MUST still resolve once against the thread working directory, and the command runs there. The runner MUST NOT add the command directory to the writable roots: only the caller-declared roots confine the command.

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

A remote box MUST ignore the sandbox mode. The box is the sandbox. A request that reaches the exec server over the websocket box channel MUST NOT weaken the sandbox below the server start mode, and the server MUST fail the request closed with `-32602` when it does.

## Daemon socket visibility

The daemon socket (`<home>/dalgona.sock`, mode `0600`) MUST stay out of sandboxed child environments: the runner MUST strip `DALGONA_HOME` and `DAL_HOME` from them, so the child cannot read the socket path from its environment. `off` MUST keep the parent environment unchanged.

This hiding is best-effort, not a boundary. Landlock filesystem rules do not mediate `connect(2)` to an existing pathname unix socket: under this policy a denied write outside the roots still leaves a connect to a pathname socket there working. Landlock scoping covers abstract unix sockets only, and the daemon socket is a pathname socket. `HOME` stays in the child environment, so `$HOME/.dalgona/dalgona.sock` remains guessable, and reads are allowed on every path. A sandboxed child of the same uid can therefore still dial the socket. The harness sandbox only keeps commands from writing outside the workspace by accident; it is not a security boundary.

## Failure

The shim MUST apply the policy before it starts the command. A policy error MUST write the error to `out`. The call MUST exit nonzero. The shim MUST still write `exit` and perform the configured `DALGONA_SYNCFS` operation.
