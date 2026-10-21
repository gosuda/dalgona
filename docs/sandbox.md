# Sandbox dal exec commands: Landlock, Seatbelt, AppContainer

Run every `exec` child inside the platform sandbox. A command can write only under the workspace and a few other roots, so it cannot change the rest of your home directory. Turn it on with `--sandbox` or `sandbox = true`. It does not restrict the network.

Use it when you let the model run commands. The sandbox is off by default. It does not cover plugins, which run with your permissions.

## Turn it on

Pass `--sandbox` for one run. To keep it on, set it in `dal.toml`. Add each folder that a command must write to with `sandbox_writable`:

```sh
dal --sandbox
```

```toml
sandbox = true
sandbox_writable = ["~/.cargo"]
```

Each `sandbox_writable` entry is an absolute path or starts with `~/`. Anything else makes dal refuse the command with `sandbox_writable: "<entry>" must be an absolute path or start with "~/".`

## Check that it works

At session start dal shows `Sandbox on. Commands can write only under: <roots>.` Ask the model to run `touch "$HOME/sandbox-probe"`. Approve the call. The command fails with a permission error. Dal then appends this note to the result:

```text
dalgon sandbox: a "Permission denied" or "Operation not permitted" error can come from the sandbox; if the path should be writable, add it to sandbox_writable in dal.toml.
```

## What it allows

The writable roots are the workspace, the temporary directory, the platform cache directory, and the `sandbox_writable` entries. Dal captures its environment at startup, and later changes do not move the roots. On Unix the temporary directory is `TMPDIR`, or `/tmp` if `TMPDIR` is absent or invalid. On Windows dal checks `TMP`, then `TEMP`, then `AppData/Local/Temp` under the captured home directory. Only non-empty absolute paths are valid. Nothing else under the home directory is writable.

## What it does not cover

- It sets no network restriction.
- It does not cover plugins. A plugin runs with your permissions.
- It does not replace approval. By default, `exec` still asks before it runs.

## Platforms and setup errors

The helper is Landlock on Linux through `dalgon __sandbox`, Seatbelt on macOS, and an AppContainer on Windows. If the helper is missing or the system cannot enforce the sandbox, dal refuses the command with the setup error. It never runs the command unsandboxed.

| platform | error text | fix |
|---|---|---|
| Linux | `sandbox = true needs Landlock ABI 3 (Linux 6.1 or newer); this kernel reports ABI <n>.` | Use Linux 6.1 or newer, or turn the sandbox off. |
| macOS | `sandbox: command launches need /usr/bin/sandbox-exec, and it is missing.` | Restore `/usr/bin/sandbox-exec`. Every command launch uses it, so turning the sandbox off does not help. |

In `dal.toml`, `sandbox` is `true` or `false`, not a string.

On Windows the backend matches the read-open model as far as DACL editing safely reaches. The per-run `dalgon.sandbox.<guid>` container reads and executes wherever system ACLs already permit application containers (`ALL APPLICATION PACKAGES`). It also reads and executes in the launch directory and every `PATH` directory. Each writable root's ancestors get traverse access only (POSIX `+x` parity). The container resolves through them. It cannot list or read sibling files. A failed grant there fails the launch. A partially installed read policy is worse than none. Drive roots are never touched. An inheritable ACE on a drive root would propagate to every file on the volume. Writes stay under the granted roots, which get full control.

## Next

- [Tools, modes, and approval](tools.md) explains the approval ladder that `exec` passes first.
- [Settings in dal.toml](config.md) lists `sandbox` and `sandbox_writable`.
