# Protocol

The protocol is a subset of the Codex app-server v2 protocol with dalgona extensions. `dal` serves it in process over `net.Pipe`. `dalgona` serves it on its socket, on stdio, and on websocket.

## Framing

The transport MUST use one JSON object per line. The server MUST accept requests without a `jsonrpc` member. The server MUST emit `"jsonrpc":"2.0"`. The server MUST NOT accept a request line longer than 16 MiB.

Each request has `id`, `method`, and optional `params`. Each response has the same `id` and either `result` or `error`. The server MUST ignore unknown parameter members.

Error codes:

| Code | Use |
|---|---|
| `-32700` | Parse error. The line is not valid JSON. The `id` is `null` when it cannot be determined. |
| `-32600` | Invalid request. Valid JSON whose envelope is not a request, for example a non-string `method`, and thread state conflicts such as `thread <id> is running` or `turn <id> is not running`. |
| `-32601` | Unknown method. Message: `Method not found`. |
| `-32602` | Invalid or unknown parameters, for example a wrong parameter member type or `unknown thread <id>`, `turn <id> is not on the active path`, `unknown turn <id>`, `unsupported approvalPolicy <value>`, `unsupported keyPath <value>`. |
| `-32603` | Internal failure, and every handler error without a more specific code. The message is the cause. |
| `-32001` | `advisor runtime is not available`. |

A connection MUST run one writer. The writer queue MUST hold at most 4096 messages. Delta notifications MAY merge only while the queue holds 1024 or more messages. Delta notifications for `item/agentMessage/delta`, `item/reasoning/textDelta`, and `item/commandExecution/outputDelta` MAY merge into the newest queued delta for the same item. The merge MUST keep the order of the text of that item. A merged delta MAY reach the client before a message that was queued between the two deltas. Other messages MUST NOT be dropped or merged. A connection with 4096 queued messages, deltas included, MUST close and log `rpc: closing slow client <addr>`.

The wire supports concurrent requests in both directions. Request identifiers MUST route responses to the waiting caller. The transport MUST close when its stream closes. Calls that wait at that time MUST return a closed-pipe error. Cancellation of a caller context MUST remove the pending request.

## Schema

Every response and notification in the Codex subset MUST validate against the vendored schema in `internal/rpc/testdata/codex-schema` (Codex commit `c9b4267c370a755da12ec1c8648c2451163212b7`). Extension methods and notifications under `dalgona/` MUST validate against `internal/rpc/testdata/dalgona-schema`. The server encodes every message with `encoding/json/jsontext` or `encoding/json/v2`.

## Client guarantees

A client MAY validate every message against the schemas. A client MUST ignore an unknown member and an unknown notification method.

- Every response, notification, and server request in the Codex subset MUST validate against `internal/rpc/testdata/codex-schema`.
- Every `dalgona/` message MUST validate against `internal/rpc/testdata/dalgona-schema`.
- Every `thread/status/changed` MUST carry `seq`, an integer of at least 1.
- Per connection and thread, `seq` MUST strictly increase. This holds after a slow-subscriber catch-up.
- A client MUST drop a status whose `seq` is lower than the last status it applied for that thread.
- Notifications of one thread MUST arrive in journal order.
- `item/started` MUST precede the deltas and `item/completed` of the same item.
- `item/fileChange/patchUpdated` for an edit approval MUST precede its `item/fileChange/requestApproval`.
- `warning` MUST precede the sandbox choice request.
- A server request MUST close with `serverRequest/resolved` when another client or the server settles it.
- Text, diffs, and outputs MUST be masked before they leave the server. The exceptions are `command/exec` output and `fs/readFile` data.

## Startup

`initialize` MUST be answered without waiting for recovery or for any thread. The result is `{"userAgent":"<product>/<version>","codexHome":"<home>","platformFamily":"unix|windows","platformOs":"macos|linux|windows"}`. `codexHome` is the product home. The server ignores `codexHome` and `CODEX_HOME` from the client. The server accepts the `initialized` notification and does nothing.

`Serve` MUST NOT run host recovery. `dalgona` runs recovery once at daemon start. `dal` recovers a thread when the thread opens.

## Methods

| Method | Result |
|---|---|
| `thread/start {model?, modelProvider?, cwd?, approvalPolicy?, sandbox?, config?}` | `ThreadStartResponse`. The server also sends `thread/started`. A missing `model` takes the model from the current settings. A `model` without `/` is a Codex bare id: the server prefixes it with `modelProvider`, or else with the provider of the current settings model. A missing `approvalPolicy` or `sandbox` takes the current settings value. `config.model_reasoning_effort` sets the thread's default reasoning effort: every turn of the thread that sends no `effort` uses it, and a turn's own `effort` wins. It is journaled with the thread, so `thread/resume` and `thread/fork` keep it, and the response reports it as `reasoningEffort`. A non-empty value outside `low`, `medium`, `high`, `xhigh`, `max` MUST give `-32602` `unsupported model_reasoning_effort <value>`. |
| `thread/resume {threadId}` | `ThreadResumeResponse` with all turns of the active path. The snapshot MUST replay the full active path, including history before a compaction marker. |
| `thread/list {cursor?, limit?, archived?}` | Threads ordered by `updatedAt`, newest first. `archived` selects archived threads. `limit` defaults to 50. `nextCursor` is the offset of the next page. |
| `thread/read {threadId, includeTurns?}` | `{thread}`. Turns are present only with `includeTurns`. |
| `thread/name/set {threadId, name}` | `{}`. Subscribers receive `thread/name/updated`. |
| `thread/archive`, `thread/unarchive {threadId}` | `{}` and `{thread}`. Subscribers receive `thread/archived` or `thread/unarchived`. |
| `thread/compact/start {threadId}` | `{}` after the compaction. |
| `thread/revert {threadId, beforeTurnId}` | `{thread}` with no turns. Subscribers receive `thread/reverted`. |
| `thread/fork {threadId, lastTurnId?}` | `ThreadForkResponse`. The server also sends `thread/started` for the fork. |
| `turn/start {threadId, input, effort?, model?, approvalPolicy?, sandboxPolicy?}` | `{turn}` with status `inProgress`. `effort` overrides the reasoning effort for this turn only. `model`, `approvalPolicy`, and `sandboxPolicy` override this turn and later turns on the active path. A missing value keeps the thread value. A turn `approvalPolicy` MUST NOT lower the thread mode: the server applies the stricter of the thread mode and the mapped turn mode, in the order `yolo` < `auto` < `ask`. A bare `model` id takes the provider of the thread model. The server MUST accept an `image` input item only with a `data:` URL. The server MUST accept a `localImage` input item only with an absolute path. The server MUST ignore the `detail` member of an image input item. The server MUST read a local image file that the daemon user can read. The server MUST return `-32602` for an image that is missing or has the wrong type. |
| `turn/steer {threadId, expectedTurnId?, input}` | `{turnId}`. When `expectedTurnId` is set, it MUST name the running turn. The server MUST check the target atomically with submission. A steer for a turn that is not running MUST fail and MUST NOT start a turn. The server MUST reject such a steer before it resolves tools or asks for workspace trust. The server MUST accept the same image input items as `turn/start`. |
| `turn/interrupt {threadId, turnId}` | `{}`. |
| `review/start {threadId, target, delivery?}` | `{reviewThreadId, turn}`, with `turn.status` `inProgress`. `target.type` MUST be `uncommittedChanges`, `baseBranch` with `branch`, `commit` with `sha` and an optional `title`, or `custom` with `instructions`. Another `target.type` MUST give `-32602` `unsupported review target <type>`. The review runs as an ordinary turn on `threadId` with a fixed review prompt. `delivery` MUST be absent, null, or `inline`. Another `delivery` value, including the empty string, MUST give `-32602` `unsupported delivery <d>`. A branch MUST match `^[A-Za-z0-9._/][A-Za-z0-9._/-]*$`, and a sha MUST be 4 to 64 hex digits. A rejected request MUST NOT start a turn. |
| `model/list` | Models of every configured provider. `id` is `<provider>/<model>`. Each model reports `contextWindow`, its `provider`, and `presets`: the roles `default`, `smol`, and `slow` whose preset names the model, in that order. Each model MUST report `inputModalities`. A model that accepts images MUST report `["text","image"]`. Every other model MUST report `["text"]`. |
| `config/read` | `{config: {model, model_provider, approval_policy, sandbox_mode, model_reasoning_effort}, origins: {}}` from the current settings. |
| `config/value/write {keyPath, value}` | Writes `settings.toml` through `Host.WriteSettings`, which applies the value to the running host at once. Codex keys `approval_policy`, `sandbox_mode`, and `model_reasoning_effort` map to `approval`, `sandbox`, and `reasoning`. Direct keys `model`, `approval`, `sandbox`, and `reasoning` pass through with the same value checks. The interactive keys of `dalgona/settings/read` pass through the same checks. Other keys give `-32602`. `version` is the SHA-256 of the written file. |
| `config/batchWrite {edits: [{keyPath, value, mergeStrategy}], filePath?, expectedVersion?, reloadUserConfig?}` | Applies every edit through `Host.WriteSettings`, or none when one edit is invalid. `mergeStrategy` MUST be `replace` or `upsert`, and each edit sets its key. `filePath`, when set, MUST equal the settings path. `expectedVersion`, when set, MUST equal the SHA-256 of the current file, or the server answers `-32600`. `reloadUserConfig` has no effect. Result `{filePath, status: "ok", version}`. |
| `dalgona/settings/read {}` | `{settings: [{key, value, defaultValue, options}]}`. `dalgona` MUST list seven settings in this order: `approval`, `verbosity`, `theme`, `icons`, `statusHide`, `editReview`, `notifications`. `dal` MUST list the same order without `editReview`. Writes use the same keys with `config/batchWrite`. |
| `account/read` | `chatgpt` when a ChatGPT OAuth credential exists, else `apiKey` when any credential exists, else `null`. `requiresOpenaiAuth` is `false`. |
| `account/login/start {type, apiKey?, provider?}` | `apiKey` checks the key with the model list of `provider` and stores the key only when the provider accepts it. A runner service MUST store the key without a check. The key MUST be one line. A rejected key gives `-32602` `<provider> did not accept the API key; it was not saved` and changes no stored credential. `chatgpt` and `chatgptDeviceCode` run the ChatGPT OAuth flow and send `account/login/completed`. Other OAuth providers MUST log in through the terminal (`<product> login <provider>`). |
| `account/login/cancel {loginId}` | `{status: canceled|notFound}`. |
| `account/logout {provider?}` | `{}`. Without `provider`, exactly one credential MUST exist. |
| `dalgona/account/providers/list {}` | `{providers: [{id, login, signedIn}]}`, sorted by `id`. `login` is `apiKey` for the API-key providers and the runner services, `browser` for `chatgpt`, and `terminal` for the other OAuth providers. |
| `skills/list` | One entry with the host skills. `dal` returns `{"data":[]}`. |
| `mcpServerStatus/list {cursor?, limit?, serverName?, threadId?}` | `{data: [{name, authStatus, tools, resources, resourceTemplates, runtimeStatus, toolsError?}], nextCursor?}`. `limit` defaults to 100 and MUST be at most 1000. `threadId` selects the workspace of that thread. Tool names drop the `<server>__` prefix. |
| `command/exec {command, cwd?, env?, timeoutMs?, disableTimeout?, outputBytesCap?, disableOutputCap?, processId?, streamStdoutStderr?, sandboxPolicy?}` | `{exitCode, stdout, stderr}`, where `stderr` is empty and stdout and stderr arrive merged. A `command/exec` call has no thread. The server MUST run the command under the host's configured sandbox setting and MUST NOT ask for approval. The server MUST ignore a `sandboxPolicy` member except `externalSandbox`, so a client cannot turn the sandbox off for one command. `cwd` defaults to the configured working directory. `timeoutMs` defaults to 600000, and a timeout gives exit code 124. A negative `timeoutMs` MUST give `-32602` `timeoutMs must not be negative`. `outputBytesCap` defaults to 1 MiB. An `outputBytesCap` outside 1 to 67108864 MUST give `-32602` `outputBytesCap must be from 1 to 67108864`. At most 16 commands MAY run on one connection, and a command beyond that MUST give `-32602` `too many running commands`. With `streamStdoutStderr`, the server MUST send `command/exec/outputDelta` notifications for the process before it answers, and `processId` MUST be set. `tty`, `streamStdin` and `size` are not supported. |
| `command/exec/terminate {processId}` | `{}`. The server MUST send a termination signal to the running process with that `processId`, and it MUST NOT wait for the process to end. An unknown process gives `-32602` `unknown process <id>`. |
| `dalgona/keywords/list` | `[{word, hue:[a,b]}]` for enabled keywords. |
| `dalgona/thread/tree {threadId}` | `{turns:[{turnId, parentTurnId, preview, active}]}`. `parentTurnId` is `null` for a first turn. |
| `dalgona/thread/leaf/set {threadId, turnId}` | `{}`. |
| `dalgona/advisor/status {threadId}` | `[{name, model, status: idle|reviewing|halted|disabled, notes, usage:{inputTokens, outputTokens}}]`. |
| `dalgona/advisor/set {threadId, enabled}`, `dalgona/advisor/reset {threadId}` | `{}`. |
| `dalgona/item/output/read {threadId, itemId, offset?}` | `{text, offset, next, total}`. `next` equals `total` at the end of the output. A call without a spill MUST give `-32602` `call <id> has no spilled output`. |
| `dalgona/keywords/match {threadId?, text, kind?}` | `{matches:[{word, byteRange:{start, end}, hue, effort?, summary}]}`, sorted by start. `kind` is `prompt` (the default) or `steer`. Text over 64 KiB MUST give `-32602` `text exceeds 64 KiB`. |
| `fs/readFile {path}`, `fs/writeFile {path, dataBase64}`, `fs/createDirectory {path, recursive?}` | `{dataBase64}`, `{}`, `{}`. See File methods. |
| `fs/getMetadata {path}`, `fs/readDirectory {path}` | `{isDirectory, isFile, isSymlink, createdAtMs, modifiedAtMs}`, `{entries: [{fileName, isDirectory, isFile}]}`. See File methods. |
| `fs/remove {path, recursive?, force?}`, `fs/copy {sourcePath, destinationPath, recursive?}` | `{}`. See File methods. |
| `fs/watch {watchId, path}`, `fs/unwatch {watchId}` | `{path}`, `{}`. The server sends `fs/changed` notifications for a watch. See File methods. |
| `fuzzyFileSearch {query, roots, cancellationToken?}` | `{files: [{root, path, file_name, match_type, score, indices}]}`, at most 50 results. See Fuzzy file search in `search.md`. |
| `dalgona/retry/wait {threadId, turnId, remainingMs}` | Notification, not a method. Sent once a second during an attended rate-limit wait, and once with `remainingMs` 0 when the wait ends. Not journaled. |

`thread/revert`, `thread/fork`, `turn/start`, and `dalgona/thread/leaf/set` require an idle thread. A running thread MUST give `-32600` `thread <id> is running`.

A client MUST handle `account/login/completed` as follows. On `success` true, it MUST close the login panel, show `Signed in to <provider>`, and request `model/list` again. On `success` false, it MUST keep the panel open and show `error` beside the key field. A client MUST NOT promise that a turn resumes after a login.

A product MAY add methods. Built-in methods take precedence over added methods. A product MAY run an archive hook on an idle thread. The hook MUST run after `Host.Archive` journals `thread.archived`. If the hook fails, the server MUST unarchive the thread. The server MUST return error `-32603` with the message `archive hook: <cause>`.

### Input images

The server MUST accept at most 10 images in one request.
The server MUST accept an image of at most 5242880 bytes.
The server MUST accept at most 10485760 bytes of image data in one request.
The server MUST accept only PNG, JPEG and GIF image data.
The server MUST detect the image type from the first bytes of the data.

## File methods

The `fs/*` methods MUST take absolute paths. A relative path MUST give `-32602` `path must be absolute`. A path that does not exist MUST give `-32602` `no such file: <path>`.

`fs/readFile` MUST return `{dataBase64}`. `fs/readFile` and `fs/writeFile` MUST refuse a file over 16 MiB with `-32602` `file larger than 16 MiB`.

`fs/readFile` MUST refuse a path that is not a regular file with `-32602` `not a regular file: <path>`. `fs/writeFile` MUST give `-32602` `no such file: <path>` when the parent directory does not exist.

`fs/writeFile` MUST write through a temporary file in the same directory and then rename it over the path. A new file MUST have mode `0o644`. An existing file MUST keep its mode. Invalid base64 MUST give `-32602` `dataBase64 is not valid base64`.

`fs/createDirectory` and `fs/remove` MUST treat `recursive` as true when it is absent. `fs/remove` MUST treat `force` as true when it is absent. With `force` false, a missing path MUST give `-32602` `no such file: <path>`.

`fs/getMetadata` MUST take `isSymlink` from the link itself. It MUST take the other flags and times from the link target. A dangling link MUST report its own times, and neither a file nor a directory.

`fs/readDirectory` MUST list entries sorted by name. A path that is not a directory MUST give `-32602` `not a directory: <path>`.

`fs/copy` MUST copy a directory only when `recursive` is true. Otherwise it MUST give `-32602` `copying a directory needs recursive`.

`fs/watch` MUST resolve links in `path`. It MUST watch the entry of a file, or the direct entries of a directory. It MUST refuse a watch id that already exists, a connection with 64 watches, and a directory with more than 10000 entries.

`fs/watch` MUST poll each watch once a second. A poll MUST send `fs/changed {watchId, changedPaths}` for each path whose size, mode, or modification time changed, or that appeared or vanished. A message MUST name at most 1000 paths.

One poll MUST stat at most 20000 entries across all watches. A watch whose poll would exceed that budget MUST poll again after 5 seconds.

`fs/unwatch` MUST stop the watch. An unknown watch id MUST give `-32602` `unknown watch <id>`.

Any other OS failure MUST give `-32603` with the cause.

## Policy mapping

`approvalPolicy`: `never` maps to `yolo`. `on-request` and `on-failure` map to `auto`. `untrusted` and any `granular` object map to `ask`. Another object MUST give `-32602`. `sandbox`: `danger-full-access` maps to `off`. `workspace-write` and `read-only` map to `workspace-write`. `sandboxPolicy.type`: `dangerFullAccess` maps to `off`. `workspaceWrite` and `readOnly` map to `workspace-write`. `externalSandbox` MUST give `-32602` `unsupported sandboxPolicy externalSandbox`. Responses report `yolo` as `never`, `auto` as `on-request`, and `ask` as `untrusted`. Responses report `off` as `dangerFullAccess`, else `workspaceWrite`. The `model` of every thread object MUST be the effective model of the active path. `thread/start`, `thread/resume`, and `thread/fork` MUST also report the effective approval and sandbox. They MUST NOT report the `session.created` values. The effective sandbox is `off` when the configured policy is not `off` and the runner reports no harness sandbox. The server gets this value from the agent layer.

`thread/start`, `thread/resume`, and `thread/fork` MUST carry `tokenUsage` once the thread has recorded usage. `tokenUsage.modelContextWindow` MUST be present when the context window of the model is known.

## Server requests

The server sends approval requests only for threads in approval mode `ask`, plus one workspace trust prompt per untrusted workspace. An exec call uses `item/commandExecution/requestApproval`. An edit call uses `item/fileChange/requestApproval`. A workspace trust prompt reuses `item/commandExecution/requestApproval`. It sets `cwd` to the workspace path. It sets `command` to the server commands and env, joined by newlines. It sets `reason` to the full workspace prompt. The request `id` is the approval request id of the journal. `itemId` is the tool call id, empty for workspace trust.

The server MUST send `item/fileChange/patchUpdated` for the item before an edit approval request. Its `changes` carry the unified diff and the line counts of each file that the call would change. A file over the diff limit has `diffOmitted` set and no counts.

Decisions `accept`, `acceptForSession`, `decline`, and `cancel` map to the host replies. The decision `approved` maps to accept, `approved_for_session` maps to acceptForSession, and `abort` maps to cancel. An object decision MUST count as `accept`. Other values MUST count as `decline`. When the approval resolves, every subscriber receives `serverRequest/resolved {threadId, requestId, resolution}` and the server cancels its pending request. A resolved approval MUST report `answered` for an accept and `declined` for a decline. A cancel by the user MUST report `cancelled`, and a cancel that an interrupt caused MUST report `interrupted`. Any other close MUST report `declined`. When a client fails to answer and it is the last subscriber, the server MUST decline. A client MUST handle each server request on its own task. An open prompt MUST NOT block notification delivery. A client MUST cancel that task when the matching `serverRequest/resolved` arrives.

The server sends `item/tool/requestUserInput` for an `ask` call. The request `id` is a question request id that the host generates, and the host does not journal it. `itemId` is the call id. Question ids are `q1` to `q4` by position.

A client MUST answer with `answers` that maps each question id to a list of strings. An answer equal to an option label is that option. Any other string is typed text.

A client MUST answer `{"answers": {}}` to dismiss the questions. Any other error response MUST mean that the window cannot answer.

The server MUST decline a question with the no-client result when every attached window cannot answer. The first answer MUST decide the question, and every subscriber receives `serverRequest/resolved` with a `resolution` of `answered`, `cancelled`, `interrupted` or `declined`.

The sandbox choice uses `item/tool/requestUserInput` with the header `Sandbox`, the options `Accept` and `Dangerously accept`, and no Other row. The server sends a `warning` notification before the question. A cancelled question MUST decline the command, and the next command MUST ask again. After a choice is journaled, the server MUST send `dalgona/sandbox/chosen` with `threadId`, `cause`, and `choice`.

## Notifications and items

`waitingOnUserInput` is an active flag that the server sets while a question is open. It carries no journal event.

Each connection subscribes to every thread that it starts, resumes, or changes. The server translates journal events and stream deltas:

`thread/status/changed` carries the extension member `seq`: the journal sequence number of the event that changed the status. A status without a journal event MUST carry the latest applied journal `seq`, or the last sent `seq` plus one when greater. A client MUST ignore a status with a lower `seq` than one it already applied.

Turn objects carry `startedAt` and `completedAt` as Unix seconds, and `durationMs` in milliseconds on `turn/completed`. A `commandExecution`, `mcpToolCall`, or `dynamicToolCall` item MUST carry `durationMs` when the journal holds both of its times. The server computes each duration from the journal times `started_at` and `completed_at`, so a replay shows the same values as the live stream.

| Source | Notification |
|---|---|
| `turn.started` | `turn/started`, `thread/status/changed` active |
| `turn.completed` | `error` when the turn failed, `turn/completed`, `thread/status/changed` idle |
| `turn.judged` | `dalgona/turn/judged {threadId, turnId, verdict, summary, paths}`, live only |
| `user.message` | `item/started` and `item/completed` with a `userMessage`. The server MUST echo each image of a user message as an `image` input item. The echoed image item MUST NOT carry image bytes. The echoed image item MUST name the blob with the `dalgona-blob:` prefix. |
| text delta | `item/started` once, then `item/agentMessage/delta` |
| reasoning delta | `item/started` once, then `item/reasoning/textDelta` for item `<item>-reasoning` |
| `assistant.message` | `item/completed` for the `agentMessage` and the `reasoning` item, `thread/tokenUsage/updated` |
| `approval.requested` | `item/started` for the call when not started, `thread/status/changed` with `waitingOnApproval` |
| `approval.resolved` | `serverRequest/resolved` |
| `tool.started` | `item/started` |
| `tool.completed` | `item/completed`. For the `todo` tool, also `turn/plan/updated` |
| `tool.judged` | `dalgona/tool/judged {threadId, turnId, itemId, verdict, summary, paths}`, live only |
| `compaction` | `contextCompaction` item |
| `model.retry` | `error` with `willRetry: true` |
| `thread.named` | `thread/name/updated` |
| `thread.archived` | `thread/archived` or `thread/unarchived` |
| `branch` | `thread/reverted` |
| `child.spawned` | `dalgona/thread/childSpawned {threadId, childId, cwd}` |
| `advisor.note` | a completed `dynamicToolCall` item with tool `advisor`, and `dalgona/advisor/note {threadId, advisor, severity, note, delivery}` |

The tool class sets the item kind. Class `exec` gives `commandExecution`. Class `edit` gives `fileChange` with one change per file. Class `remote` gives `mcpToolCall` with the tool name `<server>__<tool>` after the MCP name rule. Any other tool gives `dynamicToolCall`. A call that completes without a start, with a `decline` or `cancel` decision, has status `declined` for `commandExecution` and `fileChange`. For other kinds, that call has status `failed`. Its output is the decline reason.

A file change carries a unified diff of its journaled pre-image and post-image, with three lines of context. `additions` and `deletions` count the added and removed lines. A file without a pre-image is `add`. A file without a post-image is `delete`.

A file change MUST set `diffOmitted` to `true` when either image is over 1 MiB. It MUST also do so when an image has a digest but no blob, or when its blob cannot be loaded. An update record written before post blobs existed has a post digest and no post blob. Such a change MUST have an empty `diff` and no counts.

`commandExecution`, `mcpToolCall`, and `dynamicToolCall` items MUST carry `outputSpill` when their output is spilled. `outputSpill` MUST hold `path`, `bytes`, and `sha256`, and the client MUST read the rest of the output with `dalgona/item/output/read`. A page MUST hold at most 1 MiB of source bytes, and it MUST end at a line break when one lies in its window. Without a line break, a page MUST end at a UTF-8 rune start, or at the byte limit if none exists.

When a subscription closes because the subscriber was slow, the server MUST subscribe again and replay the missed events from the journal in sequence order. An event with a sequence number at or below the last delivered one MUST NOT be sent twice.

A `userMessage` item MUST carry `keywords` when its prompt fired notices. Each entry MUST hold the match that `dalgona/keywords/match` returns for that prompt, with its `byteRange` into the item text. The server MUST match keywords without checking required tools until the thread has resolved its tools.

## Clients

`dal` and `dalgona` share one command line (`internal/app` `RunCLI`):

- `-p <prompt>` runs one turn and exits. Without `--json` it prints the agent messages of the turn. With `--json` it prints `{"thread":"<id>","status":"completed|interrupted|failed","stop_reason":"..."}`. `stop_reason` is `stop` for a completed turn, the turn error for a failed turn, and `interrupted` for an interrupted turn. The exit code is 0 only for a completed turn.
- `--resume <id>` and `-c` (latest thread) select a thread. With `-p ""`, the command waits for a recovered turn and does not start a new turn.
- `-m`, `--approval auto|ask|yolo`, `--sandbox workspace-write|off` apply to a new thread. `-C <dir>` changes the working directory first.
- In `-p` mode the client declines every approval request. It answers each question with an error. The server declines a question only when no other attached window can answer it. The model then receives `declined: no client could answer this question`.
- `login [provider] [--device-auth]`, `logout <provider>`, and `models` complete without the TUI. `login` without a provider lists the providers.

`app.TUIOptions.OnStatus` MUST receive `app.Status` values that `cmd/internal/tuiwire` converts from `tui.Status`. `internal/app` MUST NOT import `internal/tui`, so command entry points set `CLIEnv.TUI`.
