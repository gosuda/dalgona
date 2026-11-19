#### Schemas of the protocol workstream

Casing: wire camelCase, journal and config snake_case, journal kinds dotted lowercase, extensions under `dalgona/`. "Ext" marks an extension member on a Codex object; every Codex object touched is open (no `additionalProperties: false` in the vendored schema).

##### Client guarantees (protocol.md, step 1)

- G1. Every response, notification and server request of the Codex subset validates against `internal/rpc/testdata/codex-schema`; every `dalgona/` message validates against `internal/rpc/testdata/dalgona-schema`. A client MAY validate every message and MUST ignore unknown members and unknown notification methods.
- G2. Every `thread/status/changed` carries `seq` (uint64, at least 1). Per connection and thread, `seq` strictly increases, including after a slow-subscriber catch-up. A client MUST drop a status whose `seq` is not greater than the last one it applied for that thread.
- G3. Notifications of one thread arrive in journal order; `item/started` precedes deltas and `item/completed` of the same item.
- G4. `item/fileChange/patchUpdated` for an edit approval precedes its `item/fileChange/requestApproval`; `warning` precedes the sandbox choice request.
- G5. A server request closes with `serverRequest/resolved` when another client or the server settles it.
- G6. Text, diffs and outputs are masked before they leave the server, except `command/exec` output and `fs/readFile` data.

##### Journal

| Event | New field | Type | Rule |
|---|---|---|---|
| `turn.started` | `started_at` | RFC 3339 UTC string | optional; stamped at append |
| `turn.completed` | `completed_at` | RFC 3339 UTC string | optional |
| `tool.started` | `started_at` | RFC 3339 UTC string | optional |
| `tool.started` | `files[].post_blob` | blob reference | optional; masked post image |
| `tool.completed` | `completed_at` | RFC 3339 UTC string | optional |
| `assistant.message` | `context_window` | integer, at least 0 | optional |
| `sandbox.chosen` (new) | `request_id`, `choice`, `cause` | strings | all required; `choice` is `accept` or `dangerously_accept`; `cause` non-empty, at most 1 KiB; inside or outside a turn; the latest one wins |

Example: `{"type":"sandbox.chosen","request_id":"req-01J","choice":"accept","cause":"Landlock is not available on this kernel"}`.

##### Config keys (settings.toml and config.toml)

| Key | Type | Values | Default |
|---|---|---|---|
| `verbosity` | string | `low`, `medium`, `high` | `medium` |
| `theme` | string | `auto`, `auto-soft`, `night`, `soft-night`, `day`, `soft-day` | `auto` |
| `icons` | string | `auto`, `unicode`, `nerd` | `auto` |
| `status_hide` (wire `statusHide`) | string array | `effort`, `approval`, `sandbox`, `folder`, `queue`, `context`, `mark`; `model` is rejected | `[]` |
| `[edit_review]` `enabled` (wire `editReview`) | boolean | | `false` |
| `notifications` | boolean | | `true` |

Examples: `verbosity = "low"`, `theme = "auto-soft"`, `icons = "nerd"`, `status_hide = ["folder", "queue"]` and `[edit_review]` with `enabled = true`. Both products accept all seven. The terminal reads `notifications` in its desktop notification step (the terminal workstream, Step 36, `Window title and desktop notifications`) and sends a desktop notification only when the key is `true`. The server stores and serves the key and sends no notification of its own.

Settings notice (both clients, after a save of `approval` or `sandbox`): `saved; open threads keep their approval and sandbox`.

##### Extension and newly set members on Codex objects

- `Turn` (every turn object): `startedAt` int64 Unix seconds, `completedAt` int64 Unix seconds, `durationMs` int64; each omitted when unknown. Example: `{"id":"turn-1","items":[],"status":"completed","startedAt":1794391200,"completedAt":1794391212,"durationMs":11840}`.
- `commandExecution`, `mcpToolCall`, `dynamicToolCall` items: `durationMs` int64 (Codex field), `outputSpill` (ext) `{path: string, bytes: int64, sha256: 64 hex}`. Example: `"outputSpill":{"path":"blobs/9f2c…","bytes":70000,"sha256":"9f2c…"}`.
- `FileUpdateChange` (in `fileChange` items and `item/fileChange/patchUpdated`): `diff` unified diff with `--- a/<path>` and `+++ b/<path>` headers, empty when omitted; `additions` int and `deletions` int (ext), absent when `diffOmitted` is `true`. `diffOmitted` bool (ext), `true` when a side exceeds 1 MiB, when an update record written before post blobs existed has a post SHA and no post blob, or when a blob cannot be loaded; absent otherwise. Example: `{"path":"a.txt","kind":{"type":"update"},"diff":"--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-one\n+uno\n two\n","additions":1,"deletions":1}`.
- `thread/tokenUsage/updated.tokenUsage.modelContextWindow` int (Codex field, now set from the journal).
- `ThreadStartResponse` (`thread/start`, `thread/resume`, `thread/fork`): `tokenUsage` (ext, Codex `ThreadTokenUsage` shape, omitted before the first usage); `sandboxFallback` (ext) `{cause: string, choice: "accept" | "dangerouslyAccept" | null}`, present only when the thread asks for `workspace-write` on a local runner without a harness sandbox. Example: `"sandboxFallback":{"cause":"windows has no harness sandbox","choice":null}`.
- `userMessage` item: `keywords` (ext) array of KeywordMatch (below), present when the prompt fired notices.
- `model/list` data: `contextWindow` int, `provider` string, `presets` array of `default` | `smol` | `slow`. `inputModalities` is not written by this workstream. The image-input workstream, Step 6, owns that member. Example: `{"id":"openai/gpt-6.1-sol",…,"contextWindow":128000,"provider":"openai","presets":["default"]}`.
- `account/login/completed`: `provider` string (ext). Example: `{"success":false,"error":"openai did not accept the API key; it was not saved","provider":"openai"}`.
- `thread/status/changed.seq`: uint64, always present.

##### Notifications newly emitted

- `item/fileChange/patchUpdated` `{threadId, turnId, itemId, changes: FileUpdateChange[]}`. `turnId` is empty for a forwarded child approval.
- `warning` `{threadId, message}`. Message: `Sandbox unavailable: <cause>. Choose how commands in this thread run.`
- `account/login/completed` for API keys, as above. Client rule: on `success` close the login panel, show `Signed in to <provider>`, and call `model/list` again; on failure keep the panel open with `error` under the field. Never promise to resume a turn.
- `command/exec/outputDelta` `{processId, stream: "stdout", deltaBase64, capReached}`.
- `fs/changed` `{watchId, changedPaths: absolute string[]}` (at most 1000 paths per message).
- `dalgona/sandbox/chosen` `{threadId: string, cause: string, choice: "accept" | "dangerouslyAccept"}`. Example: `{"threadId":"th-1","cause":"Landlock is not available on this kernel","choice":"accept"}`.

- `dalgona/retry/wait` `{threadId: string, turnId: string, remainingMs: int64}`. Sent once per second of an attended rate-limit wait, and once with `remainingMs` 0 when the wait ends. Live only, not journaled. Example: `{"threadId":"t1","turnId":"turn-1","remainingMs":59000}`.

##### Server request: sandbox choice

`item/tool/requestUserInput`, request id = the `QuestionPrompt.RequestID` defined by the ask workstream, Step 2 (`req-<id>`, the ask workstream decision D13):

```json
{"threadId":"th-1","turnId":"turn-3","itemId":"call-7","isBlocking":true,"questions":[{"id":"q1","header":"Sandbox","question":"The harness sandbox is not available: Landlock is not available on this kernel. How should commands in this thread run?","isOther":false,"isSecret":false,"multiSelect":false,"options":[{"label":"Accept","description":"Run without a sandbox. Every command asks for your approval first."},{"label":"Dangerously accept","description":"Run without a sandbox under approval mode auto."}]}]}
```

The second description names the thread's approval mode (`auto` or `ask`). The question id is `q1`, which `QuestionID(0)` of the ask workstream, Step 2 gives. The sandbox question has no Other row (`isOther` false), so a typed answer cannot reach it. Its answers are `Accept` or `Dangerously accept`, and a cancel is the only other outcome. Response `{"answers":{"q1":{"answers":["Accept"]}}}`. A cancel, an error and a closed connection follow the ask workstream, Step 3 and decision D5, which own the cancel shape (cross-phase decision 9). Any other answer counts as no choice and declines the call.

##### Text for `docs/spec/sandbox.md`, section "No harness sandbox" (S1)

"Windows, Linux without Landlock, and macOS without `/usr/bin/sandbox-exec` have no harness sandbox; `runner.UnavailableCause()` names the reason. On a local-runner thread whose sandbox is `workspace-write`, the first `exec` call that would run confined MUST NOT start before the user chooses. The server sends `warning` and then the sandbox question. `Accept` MUST send every later exec call of the thread to the human approval path, without session grants. `Dangerously accept` MUST run exec calls without confinement under the thread's approval mode. The agent MUST journal the choice as `sandbox.chosen` and reuse it; recovery MUST NOT ask again. Without a subscribed client the call MUST be declined with `declined: no client could choose how to run without a sandbox (<cause>); set sandbox = \"off\" to run without one`. A sandbox set to `off`, and approval mode `yolo`, MUST never ask. `command/exec` is a user command and MUST NOT ask. The runner MUST NOT report an unsandboxed process as confined." The invariant added to `docs/decisions/minimal-harness-sandbox.md`: "Without a harness sandbox, an agent command MUST NOT run unconfined before the user chose so for that thread." Its "Rejected alternatives": silent fallback (the old rule) and a host-wide choice at start.

##### Codex methods

| Method | Params (fields we honour) | Result | Errors |
|---|---|---|---|
| `review/start` | `threadId`, `target` (`uncommittedChanges` / `baseBranch{branch}` / `commit{sha,title?}` / `custom{instructions}`), `delivery?` (`inline`) | `{reviewThreadId, turn}`; `turn.status` `inProgress` | `unsupported delivery <d>`, `invalid branch <b>`, `invalid sha <s>`, `instructions must not be empty`, thread errors as `turn/start` |
| `mcpServerStatus/list` | `cursor?`, `limit?` (default 100, max 1000), `serverName?`, `threadId?`, `detail?` (ignored) | `{data: McpServerStatus[], nextCursor?}`; each `{name, authStatus: "unsupported", tools: {<name>: {name, description, inputSchema}}, resources: [], resourceTemplates: [], runtimeStatus: "starting" | "connected" | "failed", toolsError?}` | `limit must be at most 1000` |
| `config/batchWrite` | `edits[{keyPath, value, mergeStrategy}]`, `filePath?`, `expectedVersion?`, `reloadUserConfig?` | `ConfigWriteResponse {filePath, status: "ok", version}` | see step 10 |
| `command/exec` | `command` (non-empty argv), `cwd?` (absolute, default `Host.Config().Cwd`), `env?` (null unsets), `timeoutMs?` (default 600000), `disableTimeout?`, `outputBytesCap?` (default 1048576, max 67108864, merged output), `disableOutputCap?`, `processId?`, `streamStdoutStderr?`, `sandboxPolicy?` (mapping of `sandboxPolicyMode`, `serve_turn.go:69-79`) | `{exitCode, stdout, stderr: ""}`; timeout gives `exitCode 124` | `command must not be empty`, `cwd must be absolute`, `tty is not supported`, `streamStdin is not supported`, `size needs tty`, `streamStdoutStderr needs a processId`, `process <id> is running`, `timeoutMs and disableTimeout exclude each other`, `outputBytesCap and disableOutputCap exclude each other`, `unsupported sandboxPolicy externalSandbox`, at most 16 processes per connection: `too many running commands` |
| `command/exec/terminate` | `processId` | `{}` | `unknown process <id>` |
| `fs/readFile` | `path` | `{dataBase64}` | `path must be absolute`, `no such file: <path>`, `file larger than 16 MiB` |
| `fs/writeFile` | `path`, `dataBase64` | `{}` | `dataBase64 is not valid base64`, `file larger than 16 MiB` |
| `fs/createDirectory` | `path`, `recursive?` (default true) | `{}` | OS cause as `-32603` |
| `fs/getMetadata` | `path` | `{isDirectory, isFile, isSymlink, createdAtMs: 0, modifiedAtMs}` | `no such file: <path>` |
| `fs/readDirectory` | `path` | `{entries[{fileName, isDirectory, isFile}]}` sorted | `not a directory: <path>` |
| `fs/remove` | `path`, `recursive?` (default true), `force?` (default true) | `{}` | missing path with `force: false`: `no such file: <path>` |
| `fs/copy` | `sourcePath`, `destinationPath`, `recursive?` | `{}` | `copying a directory needs recursive` |
| `fs/watch` | `watchId`, `path` | `{path}` (cleaned, links resolved) | `watch <id> exists`, `too many watches`, `directory has more than 10000 entries` |
| `fs/unwatch` | `watchId` | `{}` | `unknown watch <id>` |
| `fuzzyFileSearch` | `query` (1 to 256 bytes), `roots` (1 to 8 absolute directories), `cancellationToken?` | `{files[{root, path, file_name, match_type: "file", score, indices}]}`, at most 50; a request superseded by a newer one with the same token answers `{"files":[]}` | `query must not be empty`, `root <r> is not a directory` |

All parameter errors are `-32602`, `settings.toml changed` is `-32600`, OS failures other than "not exist" are `-32603` with the cause, and `no such file: <path>` is `-32602`.

Review prompts (exact):
- uncommittedChanges: `Review the uncommitted changes in this workspace: staged, unstaged, and untracked files. Use git status and git diff HEAD to see them. Report concrete defects with file and line, most severe first. Do not edit files.`
- baseBranch: `Review the changes on the current branch against <branch>. Use git diff <branch>...HEAD to see them. Report concrete defects with file and line, most severe first. Do not edit files.`
- commit: `Review the changes introduced by commit <sha><: title when set>. Use git show <sha> to see them. Report concrete defects with file and line, most severe first. Do not edit files.`
- custom: the instructions verbatim.

Fuzzy rule: lowercase both sides; the query must be a subsequence of the path (greedy left to right; `indices` are rune positions in `path`); `gaps = (last - first + 1) - len(query)`; `score = max(0, 100000 - 100*gaps - runeCount(path) + 1000*[last index in base name] + 2000*[base name starts with query])`; sort by score descending, then path ascending; at most 100000 files walked per root.

Example: `{"id":9,"method":"fuzzyFileSearch","params":{"query":"serve","roots":["/w"]}}` gives `{"files":[{"root":"/w","path":"docs/serve.md","file_name":"serve.md","match_type":"file","score":102987,"indices":[5,6,7,8,9]}]}`.

##### Dalgona methods

- `dalgona/keywords/match`: params `{threadId?: string, text: string (at most 64 KiB), kind?: "prompt" | "steer" (default prompt)}`; result `{matches: KeywordMatch[]}` sorted by start. KeywordMatch `{word: string, byteRange: {start: int, end: int} (UTF-8 byte offsets, end exclusive), hue: [int, int], effort?: "max", summary: "effort max, this turn" | "notice added"}`. Errors `text exceeds 64 KiB`, `unsupported kind <k>`. Example result: `{"matches":[{"word":"ultrathink","byteRange":{"start":7,"end":17},"hue":[0,330],"effort":"max","summary":"effort max, this turn"}]}`.
- `dalgona/settings/read`: params `{}`; result `{settings: [{key: string, value: string | boolean | string[], defaultValue: string | boolean | string[], options: (string | boolean)[]}]}` in the order approval, verbosity, theme, icons, statusHide, editReview, notifications. `statusHide` is the wire key of `status_hide`: its value and default are arrays of option strings, and its options are the hideable ids. Example item: `{"key":"theme","value":"night","defaultValue":"auto","options":["auto","auto-soft","night","soft-night","day","soft-day"]}`. Example item for `statusHide`: `{"key":"statusHide","value":["folder","queue"],"defaultValue":[],"options":["effort","approval","sandbox","folder","queue","context","mark"]}`. Writes use `config/batchWrite` with the same keys; `statusHide` is written as `keyPath` `statusHide`.
- `dalgona/account/providers/list`: params `{}`; result `{providers: [{id: string, login: "apiKey" | "browser" | "terminal", signedIn: boolean}]}` sorted by id over `auth.Providers()`; `chatgpt` is `browser`, other OAuth providers are `terminal`, and `RunnerServices` are `apiKey`. Client rule: `apiKey` shows a key field and calls `account/login/start {type: "apiKey", provider, apiKey}`; `browser` calls `account/login/start {type: "chatgpt"}` or `chatgptDeviceCode`; `terminal` shows `<product> login <id>`. Example: `{"providers":[{"id":"anthropic","login":"apiKey","signedIn":false}]}`.
- `dalgona/item/output/read`: params `{threadId: string, itemId: string, offset?: int64 (default 0)}`; result `{text: string, offset: int64, next: int64, total: int64}`; `next == total` marks the end; a page holds at most 1 MiB of source bytes. Errors `call <id> has no spilled output`, `offset <n> is outside 0..<total>`, `unknown thread <id>`. Example: `{"text":"000…","offset":0,"next":70000,"total":70000}`.

##### Host interfaces (Go)

`agent.HostConfig.SandboxCause func() string`, `agent.HostConfig.MCPStatus func(ctx context.Context, cwd string) []agent.MCPServerStatus`, `app.Batteries.MCPStatus` of the same type, `Host.Config()`, `Host.WriteSettings(...config.SettingEdit) error`, `Host.KeywordMatches(threadID, text string, kind InputKind) []KeywordMatch`, `Host.ProviderModels(ctx context.Context, provider string) ([]model.Model, error)`, `Host.Exec(ctx, id, ExecSpec)`, `Thread.ReadOutput(ctx, callID, offset)`, `Thread.SandboxFallback() (cause, choice string, err error)`.

