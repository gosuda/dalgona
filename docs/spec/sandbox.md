# Harness sandbox

Run dalgona inside a lightweight outer sandbox (container, microVM, or a cloud sandbox runner). The harness sandbox only keeps commands from writing outside the workspace by accident. It is not a security boundary.

The harness sandbox applies only to the local runner.

## Modes

`workspace-write` MUST be the default. `off` MUST disable harness confinement. Approval mode `yolo` disables confinement only after mandatory checks and sandbox choice.

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

The shim (the internal `dalgona __shim` re-exec) MUST apply this policy before it execs the command:

```
landlock.V3.BestEffort().RestrictPaths(landlock.RODirs("/"), landlock.RWDirs(roots...), landlock.RWDirs("/dev"))
```

`UnavailableCause()` MUST name the reason when the Landlock ABI is missing, and MUST return an empty string when it exists. When the go-landlock v0.11.2 ABI query reports no ABI, the runner operates without Landlock. For applicable calls the agent MUST first obtain a no-sandbox choice before running. Without a choice or when no client can choose, the call declines fail-closed. Once accepted or dangerously accepted (and for direct `command/exec` user commands, which do not ask), the command runs without Landlock. That case MUST NOT fail the command. Linux denies no socket path. On Linux the daemon refuses a sandboxed peer instead.

## macOS

On macOS, the runner MUST start the command as `/usr/bin/sandbox-exec -p <profile> -- argv...`. The profile MUST be:

```
(version 1) (allow default) (deny network-outbound (remote unix-socket (path-literal "<denied>")))... (deny file-write*) (allow file-write* (subpath "<root>"))... (allow file-write* (literal "/dev/null") (literal "/dev/zero") (regex #"^/dev/tty") (regex #"^/dev/fd/"))
```

The agent MUST add the daemon socket to the denied list for each `workspace-write` exec call on Linux and macOS. On Windows the agent MUST NOT pass a denied list. Windows cannot confine socket paths, and the write-restricted token already denies the connect. The runner MUST apply the denied list on macOS. The runner MUST pass no denied list when the mode is `off`. For each denied socket the profile MUST deny the cleaned path, the resolved path, and the canonical real-parent spelling (matches `resolveDeniedSockets`). A missing socket MUST resolve through its real parent directory. The call MUST fail when the parent directory does not resolve. Each denied path MUST escape `"` and `\` the same way a writable root does. A denied path with a control character MUST fail the command.

## Windows

On Windows, the shim MUST start a `workspace-write` command with an unelevated restricted token. The token MUST derive from the shim token through `CreateRestrictedToken`. The token MUST carry the flags `DISABLE_MAX_PRIVILEGE`, `LUA_TOKEN`, and `WRITE_RESTRICTED`. A command MAY read every path that the user can read. A write needs a grant to a restricting SID of the token.

The restricting SIDs are the logon SID, one read-only capability SID, and one capability SID for each writable root. The token default DACL MUST also grant the logon SID read and write access. This lets the command use pipes that it creates. The writable roots are the roots of `workspace-write`. A mode with no writable root MUST grant no root.

The runner MUST derive each capability SID with HMAC-SHA256 from a seed and the lower-case cleaned root path. The seed is 32 random bytes in the file `windows-sandbox-seed` under `os.UserConfigDir()` in the directory `dalgona`. The runner MUST create the seed once and MUST reuse it. A command can read the seed. The capability SIDs name no user and no group.

Before it starts the command, the shim MUST add one allow entry for the capability SID to the DACL of each root. The entry MUST grant read, write, execute, and delete rights. The entry MUST inherit to files and subdirectories of a directory root. The shim MUST keep every existing entry of the DACL. The shim MUST skip a root whose DACL already holds the entry. A root with no DACL or a null DACL already grants every SID. The shim MUST skip such a root.

The shim MUST open each root without following a reparse point. The shim MUST fail when the root is a reparse point. The shim MUST read and write the DACL through that one handle. The grants stay on the roots after the command exits. They name SIDs that no other token carries. The first grant to a large root can take long, because Windows copies the inheritable entry to every existing child.

`UnavailableCause()` MUST return an empty string when the runner can create the restricted token and start a process under it. Otherwise it MUST name the failure. Before the first restricted start the shim MUST grant the token logon SID on its window station and desktop. The grant lets the restricting SIDs pass the window-object check during process initialization. The shim MUST fail the command when it cannot read the seed, apply a grant, or create the token. The restricted token tier does not deny network access. A run that needs network denial MUST fail before it starts. The runner applies no denied socket list on Windows. The runner adds no device grant. The shim opens the standard handles before it applies the token. Programs that use MSYS2 or Cygwin runtimes can fail to start under this token. Programs that use Schannel for HTTPS can fail with `SEC_E_NO_CREDENTIALS`.

The Windows filesystem MUST support persistent ACLs.
An ACL operation failure MUST refuse the command before it starts.
The workspace-write roots include the command directory, temporary directory, user cache directory, and configured writable roots.
Read-only policy MUST ignore configured writable roots and grant no filesystem writes.
The shim MUST close its token handle after each start attempt, including a failed start.
Windows MUST refuse a nonempty denied socket list before token creation.
Native Windows acceptance remains Not proven until the Windows tests run on a matching host.

## No harness sandbox

Linux without Landlock, macOS without `/usr/bin/sandbox-exec`, and Windows without restricted token support have no harness sandbox. `runner.UnavailableCause()` names the reason. On a local-runner thread whose sandbox is `workspace-write`, the first `exec` call that would run confined MUST NOT start before the user chooses. This excludes advisor threads, which do not wait for user decisions and never ask for a sandbox choice. For child threads with an open parent, the sandbox question routes through the parent thread. The server sends `warning` and then the sandbox question. `Accept` MUST send every later exec call of the thread that reaches this choice to the human approval path, without thread grants. `Dangerously accept` MUST run exec calls without confinement under the thread's approval mode. Static rules and hook denials decide an exec call before this choice. Hook allowance and yolo acceptance MUST NOT bypass this choice. The agent MUST journal the choice as `sandbox.chosen` and MUST reuse it. For `Accept`, the agent MUST journal `sandbox.chosen` after the user answers the approval request of the same call. For `Dangerously accept`, the agent MUST journal `sandbox.chosen` when the user answers the sandbox question. A cancelled, interrupted or timed-out question journals no `sandbox.chosen`. The next exec call then asks again. A restart before the journal holds `sandbox.chosen` makes recovery ask the question again. Recovery MUST NOT ask again when the journal holds `sandbox.chosen`. Without a subscribed client the call MUST be declined with `declined: no client could choose how to run without a sandbox (<cause>); set sandbox = "off" to run without one`. A sandbox set to `off` MUST never ask. `command/exec` is a user command and MUST NOT ask. The runner MUST NOT report an unsandboxed process as confined.

## Remote

The client-side remote runner ignores local thread sandbox mode and requests mode `off`: the box is the sandbox. Confinement is governed instead by the exec server's startup floor on the websocket box channel. The server determines its fixed sandbox floor from `DALGONA_SANDBOX_MODE`. When unset or `off`, the floor is `off`. An unknown `DALGONA_SANDBOX_MODE` value normalizes to `workspace-write`.

On the box channel, the exec server MUST accept any requested sandbox mode when its start mode is unset or `off`. When the server start mode is `workspace-write`, a request MUST NOT weaken the sandbox below that floor. The server MUST refuse any request to run with mode `off` fail-closed with RPC error `-32602`. Remote and box runners carry no deny list: no daemon socket exists in a box.

## Daemon socket visibility

The daemon socket (`<home>/dalgona.sock`, mode `0600`) MUST stay out of sandboxed child environments. The runner MUST strip `DALGONA_HOME` and `DAL_HOME` from sandboxed child environments. The child cannot read the socket path from its environment. With mode `off`, the runner MUST keep `DALGONA_HOME` and `DAL_HOME` in the child environment.

This hiding is best-effort, not a boundary. Landlock filesystem rules do not mediate `connect(2)` to an existing pathname unix socket. Under this policy, a denied write outside the roots does not stop a connect to a pathname socket there. Landlock scoping covers abstract unix sockets only, and the daemon socket is a pathname socket. `HOME` stays in the child environment, so `$HOME/.dalgona/dalgona.sock` remains guessable, and reads are allowed on every path. A sandboxed child of the same uid can therefore still dial the socket. On Linux the daemon MUST refuse to serve that peer (see [daemon.md](daemon.md)). On macOS the seatbelt profile MUST deny the connect itself.

The local sandbox limits file writes. A sandboxed command can still reach other services of the same user (for example the systemd user manager or a container daemon). It is not a boundary against a hostile process. A process outside the sandbox that hands an open daemon connection to a sandboxed command is out of scope.

## Failure

The shim MUST apply the policy before it starts the command. A policy error MUST write the error to `out`. The call MUST exit nonzero. The shim MUST still write `exit`. On Linux, the shim MUST perform the configured `DALGONA_SYNCFS` operation. On other systems the variable MUST have no effect.
