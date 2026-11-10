# Local runner

The local runner MUST store one directory per call. The directory MUST be `<dir>/<call_id>/`. It MUST contain the files `out`, `pid`, and `exit`. It MAY contain `pgid` and `cancel`.

Callers MUST escape a call id that contains `/`, such as a nested `<run>/<n>` cell call id, to a single path component before `Start`. A call id with `/` MUST NOT reach the runs directory.

The parent MUST start the shim detached. The invocation MUST be `<self> __shim --dir <runs/call_id> -- argv...`. On Linux `<self>` MUST be `/proc/self/exe`, so the shim and the parent are one build when an upgrade replaces the file at the start path. On macOS and Windows `<self>` MUST be `os.Executable()`. The runner passes writable roots to the shim in `DALGONA_SANDBOX_ROOTS`. The value is an internal format between the parent and the shim of one build. On Unix the parent MUST start the shim with `Setsid`. On Windows the parent MUST start the shim with `DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP`.

`ProcessSpec.Argv[0]` equal to `@self` MUST resolve to the parent build: `/proc/self/exe` on Linux and `os.Executable()` on macOS and Windows. On a remote exec server it MUST resolve to the exec-server binary path.

`Start` MUST be idempotent. An existing call directory for the same call id MUST make `Start` return success. It MUST NOT start a second process. When `Start` fails after it creates the call directory, it MUST remove that directory, so a retry with the same call id starts the command.

The shim MUST write `pid` before it starts the command. `pid` MUST store the process id and the process start time. Recovery and `Read` MUST accept a process only when both values match. A recycled process id MUST NOT count as the shim. `Start` MUST write `pid` before it returns. The parent MUST write the same values as the shim. On Windows a write of at most 4 KiB whose staged bytes and writable state equal the existing regular file MAY keep the existing file instead of replacing it. On Windows a file replace that fails with a sharing violation or access denied MUST wait and retry for up to one second before it fails. The first `Read` after `Start` MUST report a live process when no `exit` exists.

The shim MUST start the command in a new process group on Unix. It MUST record the group id in `pgid`. On Windows it MUST assign the command to a named job object. On Windows it MUST start the command suspended. It MUST assign the suspended command to the job object. It MUST resume the command after the assignment. The shim MUST start the command on Windows with `CREATE_NO_WINDOW`. On Windows the shim MUST run a `cmd /C <command>` argv verbatim as `cmd /S /C "<command>"`. The `/S` switch MUST make `cmd` strip exactly the outer quote pair. The parent MUST reap the shim without waiting for the command in the caller.

`Read` MUST take a byte offset into `out`. It MUST return `Data`, `Next`, `Exit`, `Alive`, and `Path`. `Data` MUST be the bytes from the offset. `Next` MUST be the next unread offset. `Exit` MUST be the exit record when `exit` exists, otherwise unset. `Alive` MUST be true only when the stored pid and start time still name the shim. `Path` MUST name the `out` file. `Read` MUST check `exit` before taking the final output snapshot, and when `exit` appears after an empty check it MUST drain `out` again. A terminal response MUST carry every byte written before the exit record, across as many pages as that takes.

`Signal` MUST accept `TERM` and `KILL`. The caller MUST send `TERM` before `KILL`. On timeout the caller MUST wait 5 s after `TERM`, then send `KILL`. On Unix `Signal` MUST target the command process group. It MUST NOT signal the shim. On Windows `Signal` MUST kill the process tree through the job object. The shim MUST survive the signal and write the command exit record.

`Signal` MUST return success when `exit` exists. Otherwise it MUST write the file `cancel` before it reads `pid` or `pgid`. `cancel` MUST hold `TERM` or `KILL`. The write MUST use a temporary file and rename, then sync the call directory. A missing call directory MUST make `Signal` return `ErrProcessNotFound`. When `pid` or `pgid` does not exist yet, `Signal` MUST return success and leave the request to the shim. When `pid` names a gone process and `exit` does not exist, `Signal` MUST return `ErrProcessNotFound`. A process group that no longer exists MUST count as success. On macOS a group whose members have all exited but are not yet reaped MUST report EPERM instead of ESRCH. The runner MUST treat that EPERM as success when the group holds no live member. It MUST return EPERM when a live member remains.

The shim MUST read `cancel` after it writes `pid` and before it starts the command. If `cancel` exists, the shim MUST NOT start the command. It MUST write `exit` as for a command that the recorded signal stopped: on Unix `code` -1 and `signal` the signal text, on Windows `code` 1 for `TERM` and 137 for `KILL`. The shim MUST read `cancel` again after it writes `pgid` and assigns the job object. If `cancel` exists, the shim MUST send the recorded signal to the command itself. If that fails, it MUST kill the command process. Each side writes its own file before it reads the file of the other side. So a terminate request that arrives before `pid` or `pgid` exists MUST still stop the command, and the command MUST NOT outlive the request. `cancel` is on disk, so the request MUST survive a daemon restart.

A tool that signals a run MUST poll `Read` until `Exit` exists before the tool returns. The poll MUST stop at a bounded deadline. The tool MUST report a deadline timeout as an error.

The shim MUST pause at startup only when `DALGONA_SHIM_TEST_PAUSE` is set to `<phase>:<duration>`. Phase `pid` pauses before the shim writes `pid`. Phase `start` pauses after the first `cancel` check and before the command starts. Only tests MAY set this variable.

The shim MUST append stdout and stderr to `out`. After `out` reaches 1 GiB the shim MUST stop appending command bytes. It MUST append this line once:

```
[dalgona: output beyond 1 GiB discarded]
```

The shim MUST keep draining both pipes. The command MUST keep running until it exits. The cap is a storage limit. It is not a process limit.

The command environment MUST be the caller environment plus these defaults. Each default MUST apply only when that variable is unset:

- `CI=1`
- `DEBIAN_FRONTEND=noninteractive`
- `GIT_TERMINAL_PROMPT=0`
- `GIT_EDITOR=:`
- `GIT_SEQUENCE_EDITOR=:`
- `GIT_MERGE_AUTOEDIT=no`
- `GIT_PAGER=cat`
- `PAGER=cat`
- `EDITOR=:`
- `npm_config_yes=true`
- `PIP_NO_INPUT=1`

The shim MUST write `exit` by a temporary file and rename. `exit` MUST be JSON `{"code":N,"signal":"","ended_at":"RFC3339"}`. `code` MUST be the numeric exit code. `signal` MUST be the signal text, or empty. `ended_at` MUST be an RFC3339 timestamp.

On Linux, if `DALGONA_SYNCFS` is set, the shim MUST call `syncfs` on that path after the `exit` rename. It MUST NOT call `syncfs` before the rename completes. On other systems the variable MUST have no effect.

Crash recovery for class `Exec` MUST follow these rules. A recorded `exit` MUST complete the call with that output. A live shim MUST stay running until exit. A gone process with no `exit` MUST complete with the output so far and error `interrupted: the host restarted while this command ran; it may have partly run`. Recovery MUST set `recovered=interrupted`. Recovery MUST NOT start the command again.

File methods MUST be `ReadFile`, `WriteFile`, `Stat`, `ReadDir`, and `Remove`. `WriteFile` MUST write a temporary file and rename it. The write MUST keep the requested permission mode. On Windows `WriteFile` MUST replace a read-only destination file. The replaced file MUST keep the read-only attribute. Each method MUST stop when the context is cancelled before the operation. `ReadFile` and `Stat` MUST NOT follow a symbolic link in the final path component; the call MUST fail instead.

Output projection for `exec` MUST use `tool.Project` with `Keep` `Tail`. Output of 64 KiB or less MUST be returned whole. Larger output MUST keep the last 64 KiB. The cut MUST fall on a UTF-8 rune boundary. The projected text MUST end with this line, with `<N>` the total byte count and `<spill>` the `out` path:

```
[output truncated: <N> bytes total; full output: <spill>; page it with read offset/limit]
```
