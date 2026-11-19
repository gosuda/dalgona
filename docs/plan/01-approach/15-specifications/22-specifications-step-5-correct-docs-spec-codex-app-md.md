#### Specifications step 5. Correct `docs/spec/codex-app.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/codex-app.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 17 | Install, uninstall, and status MUST use an injectable command runner so tests can exercise launchctl and schtasks behavior without those programs. | Install, uninstall, and status MUST use an injectable command runner, so tests can run without `launchctl`. | internal/codexapp/command.go:33-36 `func commandOutput(env Env, name string, args ...string)` with the `CommandRunner` switch; no `schtasks` call in internal/codexapp (grep). `schtasks` appears only in internal/daemon/daemon.go:225 and :246, the Windows servic |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 3 | On platforms other than Windows, the installer MUST create a missing home directory with mode `0700`. | internal/auth/home_unix.go:11-13 `func ProtectHomeDir(dir string) error { return os.MkdirAll(dir, 0o700) }`; internal/codexapp/codexapp.go:100 calls `auth.ProtectHomeDir(dalgonaHome)` before `prepareRecord` |
| 7 | The installer MUST prefix only the first `Exec=` line of the desktop file. | internal/codexapp/linux.go:56-58 `if rewritten, ok := rewriteExecLine(line, exe); ok { lines[i] = rewritten; break }` |
| 13 | A missing settings file MUST start as `{}` followed by a newline. | internal/codexapp/vscode.go:42-43 `if errors.Is(err, os.ErrNotExist) { data = []byte("{}\n") }` |
| 15 | An uninstall whose install record cannot be decoded MUST fail before it changes any file. It MUST return an error that starts with `codexapp: decode install record`. | internal/codexapp/codexapp.go:122-126 `Uninstall` calls `loadRecord` and returns its error before `restoreFiles`; codexapp.go:363-378 `loadRecord` returns `codexapp: decode install record: %w` from `json.Unmarshal` |

Structure:

- Resolved, confirmed, no change (line 3, Windows home DACL before the record): internal/codexapp/codexapp.go:100 calls `auth.ProtectHomeDir` before `prepareRecord`; internal/auth/home_windows.go:24-27 `ProtectHomeDir` applies the protected DACL through `homeRootACL`/`protectHomeRoot` (home_windows.go:46-50).
- Resolved, confirmed, no change (line 19, WM_SETTINGCHANGE): internal/codexapp/registry_windows.go:15 opens `Environment`; :61-75 `broadcastEnvironmentChange` calls `SendMessageTimeoutW` with `WM_SETTINGCHANGE` (0x001a) and the string `Environment`, after both `setRegistry` and `restoreRegistry`.
- Resolved, confirmed, no change (codex record default path): internal/codexapp/codexapp.go:309-311 `dataHome` returns `DalgonaHome` or `~/.dalgona`; codexapp.go:378 `recordPath` = `<home>/codex-install.json`.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 5 | A reinstall MUST keep the prior record entries. A reinstall MUST add the prior state of each newly active target. Uninstall MUST restore every file that the installer ever changed. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 1 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/codex-app.md` prints `style findings: 0`, and `just verify` passes.

