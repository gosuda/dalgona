#### Specifications step 27. Correct `docs/spec/runner.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/runner.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 11 | When `Start` fails after it creates the call directory, it MUST record `cancel` with `KILL`, stop the shim, and stop the recorded command group before it removes that directory, so a retry with the same call id starts the command. | When the shim does not start, `Start` MUST remove the call directory, so a retry with the same call id starts the command. When `Start` fails after the shim starts, it MUST record `cancel` with `KILL`, stop the shim and the recorded command group, and then remove the call directory. | internal/runner/local.go:98-100 `removeRunDir(runDir)` after a failed startDetached; local.go:102-104 writeSpawnPIDFunc failure calls abortSpawn; local.go:124-133 abortSpawn calls requestCancel(runDir, SignalKill) at :125 and removeRunDir at :133 |
| 31 | It MUST append this line once: | It MUST append this line once, with a line break before and after it: | internal/runner/shim.go:250 `io.WriteString(w.w, "\n[dalgona: output beyond 1 GiB discarded]\n")` |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 1 | This document governs the local runner in package `internal/runner`. | internal/runner/local.go:1 `package runner` |
| 5 | `Start` MUST fail with `runner: process id must be one path component` for a call id that contains `/`. | internal/runner/local.go validateProcessSpec: rejects when filepath.Base(spec.ID) differs from spec.ID, with the error text `runner: process id must be one path component` |

Use one word for one thing.

- Keep `call directory`. Replace one directory per call at lines 3.
- Keep `call directory`. Replace `<runs/call_id>` at lines 7.

Structure:

- Scope sentence added after the title (see add).

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 11 | When the shim does not start, `Start` MUST remove the call directory. When `Start` fails after the shim starts, it MUST record `cancel` with `KILL`. It MUST then stop the shim and the recorded command group. It MUST then remove the call directory, so a retry with the same call id starts the command. |
| 11 | When `Start` cannot confirm that the command group stopped, it MUST keep the call directory. It MUST record `aborted` in that directory. A retry with the same call id MUST NOT start the command again. |
| 13 | On Windows, a write of at most 4 KiB MAY keep the existing regular file. It MAY do so only when the staged bytes and writable state equal that file. |
| 13 | On Windows, a file replace that fails with a sharing violation or access denied MUST retry. It MUST retry for up to one second. After one second, it MUST fail. |
| 17 | For a call directory that `Start` kept after an unconfirmed stop, `Read` MUST report the stored bytes. It MUST set `Alive` to false and MUST NOT return an `Exit` record. |
| 23 | The shim also signals the group when it reads `cancel`. A member can therefore be exiting when `Signal` sends its own signal. |
| 25 | It MUST write `exit` as for a command that the recorded signal stopped. On Unix, `code` MUST be -1 and `signal` MUST be the signal text. On Windows, `code` MUST be 1 for `TERM` and 137 for `KILL`. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 8 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/runner.md` prints `style findings: 0`, and `just verify` passes.

