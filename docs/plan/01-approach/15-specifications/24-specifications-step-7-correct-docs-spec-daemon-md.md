#### Specifications step 7. Correct `docs/spec/daemon.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/daemon.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 9 | The tighten step MUST reapply that DACL to the home and to every file and directory inside it. | The tighten step MUST reapply that DACL to the home and to every file and directory inside it. The tighten step MUST NOT change a symlink or an irregular entry. | home_windows.go:187-189 returns nil for `entry.Type()&fs.ModeSymlink` and `entry.Type()&fs.ModeIrregular` entries; home_windows_test.go:210 `TestProtectHomeTreeSkipsOutsideSymlink` checks that the outside DACL is unchanged |
| 43 | With `--remote <name>`, login MUST authenticate inside the box through the daemon protocol. | With `--remote <name>`, login for an API-key login target MUST store the key inside the box through the daemon protocol. With `--remote <name>`, login for `chatgpt` MUST use the device-code flow inside the box. With `--remote <name>`, login for `antigravity`, `cursor`, or `devin` MUST fail before it dials the box. The command fails with ``login <provider> cannot run inside remote <name>; run `dalgona login <provider>` here, then `dalgona cloud up <name> --copy-auth` ``. | cli.go:305-307 API-key targets send an apiKey start request; cli.go:308-309 chatgpt sends chatgptDeviceCode; cli.go:310-313 oauthProviders return the error text; cli.go:275-279 params are built before connect; login.go:18 oauthProviders; cli_test.go:168-175 pi |
| 45 | The host MUST load `<home>/plugins/*.star`, MUST log each plugin file that fails, and MUST keep the plugins that load. | The host MUST load `<home>/plugins/*.star`. It MUST log one error that names each plugin file that fails. It MUST keep the plugins that load. | plugin.go:66-79 LoadPlugins appends `plugin <name>: <err>` for each failing file and returns errors.Join; battery.go:56-58 logs that joined error once |
| 57 | While the worker is busy, only the newest status for each thread MUST wait; at most 64 pane requests MAY wait. | While the worker is busy, only the newest status for each thread MUST wait. The worker holds at most 64 pane requests. It drops any later pane request. | client.go:85-88 keeps the newest status per thread; client.go:95-97 childSpawned returns without queuing when len(w.panes) is at herdrQueueSize |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 41 | The status line MUST be one JSON object with the keys `thread`, `status`, and `stop_reason`. | cli.go:653-660 statusLine encodes exactly thread, status, stop_reason; cli.go:634-640 report prints one such line when asJSON is set |
| 43 | The API-key login targets are `anthropic`, `google`, `openai`, `openrouter`, `cloudflare`, `sandbox0`, and `sprites`. With `--api-key <key>`, login MUST store the key without a prompt. The `--api-key` option works only for the API-key login targets. For another provider, login fails with `login <provider>: --api-key works only for <targets>`. | login.go:17-22 apiKeyProviders, runnerServices and apiKeyLoginTargets; cli.go:211-232 parseLogin defines the api-key flag; cli.go:233-234 check returns `login %s: --api-key works only for %s`; cli.go:252-253 SetAPIKey on |
| 43 | With `--remote <name>`, `logout` MUST remove the provider credential inside the box through the daemon protocol. With `--remote <name>`, login without a provider MUST show the account of the box. | cli.go:366-367 logout branches to remoteLogout when Remote is set; cli.go:376-386 remoteLogout calls account/logout; cli.go:284-285 remoteLogin returns showRemoteAccount when the provider is empty; cli_test.go:155-158 pi |

Use one word for one thing.

- Keep `service`. Replace task (and `file` in the uninstall sentence) becomes `service definition` at lines 35.

Structure:

- Line 9: the symlink exception is added to the tighten sentence.
- Line 57: the semicolon and the MAY misuse are fixed, and the long sentences on the line are rewritten.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 9 | `Run` MUST create `<home>` with mode `0700`. `Run` MUST tighten an existing `<home>` to mode `0700` before it builds the host. The host therefore never reads config or the journal from a loose directory. |
| 13 | When a client with `no_new_privs` set fails its first exchange, the client MUST report that the daemon refuses clients that run inside a sandbox. |
| 23 | `Ensure` MUST tighten `<home>` to mode `0700`. `Ensure` MUST refuse a home that is not owned by the current uid. `Ensure` MUST refuse a home whose mode is not `0700`. |
| 35 | When `DALGONA_HOME` is set at install time, the unit and plist MUST pin `DALGONA_HOME` to that value. The login-time daemon therefore uses the installing client's home. |
| 57 | Inside herdr (`HERDR_ENV=1` and `HERDR_PANE_ID` set), the TUI status MUST be reported. While a turn runs, the reported status MUST be `working`. While an approval waits, the reported status MUST be `blocked`. Otherwise, the reported status MUST be `idle`. |
| 57 | On exit, the process MUST wait at most five seconds for waiting commands. After that wait, the process MUST cancel any command that still runs. Then the process MUST release the pane. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 8 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/daemon.md` prints `style findings: 0`, and `just verify` passes.

