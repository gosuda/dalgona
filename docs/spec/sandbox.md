# Harness sandbox

Run dalgona inside a lightweight outer sandbox (container, microVM, or a cloud sandbox runner). The harness sandbox only keeps commands from writing outside the workspace by accident. It is not a security boundary.

The harness sandbox applies only to the local runner.

## Modes

`workspace-write` MUST be the default. `off` MUST disable harness confinement. Approval mode `yolo` MUST force `off`.

## workspace-write

A command MAY read any path. Network access MUST stay allowed. A command MAY write only under these roots:

- the thread working directory
- `os.TempDir()`
- `os.UserCacheDir()`
- each path in `sandbox_writable_roots`

The runner MUST pass every root through `filepath.EvalSymlinks` before it applies the policy.

If a writable root does not resolve, the command MUST fail without starting.

## Working directory

An `exec` call MUST resolve a relative `cwd` against the thread working directory to an absolute path before the runner starts. When the mode is `workspace-write`, the resolved directory MUST sit inside the writable roots above. The thread working directory counts as a writable root. Otherwise, the call MUST fail without starting. In `off` (including an approved escalation) a relative `cwd` MUST still resolve once against the thread working directory, and the command runs there. The runner MUST NOT add the command directory to the writable roots: only the caller-declared roots confine the command.

## Linux

The shim MUST apply this policy before it execs the command:

```
landlock.V3.BestEffort().RestrictPaths(landlock.RODirs("/"), landlock.RWDirs(roots...), landlock.RWDirs("/dev"))
```

`Available()` MUST report whether the Landlock ABI exists. When the go-landlock v0.11.2 ABI query reports no ABI, the command MUST run without Landlock. That case MUST NOT fail the command. Linux denies no socket path. On Linux the daemon refuses a sandboxed peer instead.

## macOS

On macOS, the runner MUST start the command as `/usr/bin/sandbox-exec -p <profile> -- argv...`. The profile MUST be:

```
(version 1) (allow default) (deny network-outbound (remote unix-socket (path-literal "<denied>")))... (deny file-write*) (allow file-write* (subpath "<root>"))... (allow file-write* (literal "/dev/null") (literal "/dev/zero") (regex #"^/dev/tty") (regex #"^/dev/fd/"))
```

The agent MUST add the daemon socket to the denied list for each `workspace-write` exec call. The runner MUST apply the denied list on macOS. The runner MUST pass no denied list when the mode is `off`. For each denied socket the profile MUST deny both the cleaned path and the resolved path when they differ. A missing socket MUST resolve through its real parent directory. The call MUST fail when the parent directory does not resolve. Each denied path MUST escape `"` and `\` the same way a writable root does. A denied path with a control character MUST fail the command.

## No harness sandbox

Windows MUST always run without a harness sandbox. Linux MUST do the same when `Available()` is false. The decider state MUST carry `sandboxed=false`. The status band MUST show the `off` sandbox glyph in place of the sandbox setting. The runner MUST NOT raise an error for this fallback. It MUST NOT report a process with no harness sandbox as confined.

## Remote

A remote box MUST ignore the sandbox mode. The box is the sandbox. A request that reaches the exec server over the websocket box channel MUST NOT weaken the sandbox below the server start mode. The server MUST fail such a request closed with `-32602`. Remote and box runners carry no deny list: no daemon socket exists in a box.

On the box channel, the exec server MUST accept any requested sandbox mode when `DALGONA_SANDBOX_MODE` is unset or `off`. An unknown `DALGONA_SANDBOX_MODE` value MUST set the server start mode to `workspace-write`.

## Daemon socket visibility

The daemon socket (`<home>/dalgona.sock`, mode `0600`) MUST stay out of sandboxed child environments. The runner MUST strip `DALGONA_HOME` and `DAL_HOME` from sandboxed child environments. The child cannot read the socket path from its environment. With mode `off`, the runner MUST keep `DALGONA_HOME` and `DAL_HOME` in the child environment.

This hiding is best-effort, not a boundary. Landlock filesystem rules do not mediate `connect(2)` to an existing pathname unix socket. Under this policy, a denied write outside the roots does not stop a connect to a pathname socket there. Landlock scoping covers abstract unix sockets only, and the daemon socket is a pathname socket. `HOME` stays in the child environment, so `$HOME/.dalgona/dalgona.sock` remains guessable, and reads are allowed on every path. A sandboxed child of the same uid can therefore still dial the socket. On Linux the daemon MUST refuse to serve that peer (see [daemon.md](daemon.md)). On macOS the seatbelt profile MUST deny the connect itself.

The local sandbox limits file writes. A sandboxed command can still reach other services of the same user (for example the systemd user manager or a container daemon). It is not a boundary against a hostile process. A process outside the sandbox that hands an open daemon connection to a sandboxed command is out of scope.

## Failure

The shim MUST apply the policy before it starts the command. A policy error MUST write the error to `out`. The call MUST exit nonzero. The shim MUST still write `exit`. On Linux, the shim MUST perform the configured `DALGONA_SYNCFS` operation. On other systems the variable MUST have no effect.
