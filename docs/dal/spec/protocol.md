# Protocol

The protocol is a subset of the Codex app-server v2 protocol with dalgona extensions. `dal` serves it in process over `net.Pipe`. `dalgona` serves it on its socket, on stdio, and on websocket.

## Framing

The transport MUST use one JSON object per line. The server MUST accept requests without a `jsonrpc` member. The server MUST emit `"jsonrpc":"2.0"`. The server MUST NOT accept a request line longer than 16 MiB.

Each request has `id`, `method`, and optional `params`. Each response has the same `id` and either `result` or `error`. The server MUST ignore unknown parameter members.

Error codes:

| Code | Use |
|---|---|
| `-32700` | Parse error. The line is not valid JSON text, including malformed UTF-8 or an unpaired surrogate escape. The `id` is `null` when it cannot be determined. |
| `-32600` | Invalid request. Valid JSON whose envelope is not a request, for example a non-string `method`, and thread state conflicts such as `thread <id> is running` or `turn <id> is not running`. |
| `-32601` | Unknown method. Message: `Method not found`. |
| `-32602` | Invalid or unknown parameters, for example a wrong parameter member type or `unknown thread <id>`, `turn <id> is not on the active path`, `unknown turn <id>`, `unsupported approvalPolicy <value>`, `unsupported keyPath <value>`. |
| `-32603` | Internal failure, and every handler error without a more specific code. The message is the cause. |
| `-32001` | `advisor runtime is not available`. |

A connection MUST run one writer. The writer queue MUST hold at most 4096 messages. Delta notifications MAY merge only while the queue holds 1024 or more messages. Delta notifications for `item/agentMessage/delta`, `item/reasoning/textDelta`, and `item/commandExecution/outputDelta` MAY merge into the newest queued delta for the same item. The merge MUST keep the order of the text of that item. A merged delta MAY reach the client before a message that was queued between the two deltas. Other messages MUST NOT be dropped or merged. A connection with 4096 queued messages, deltas included, MUST close and log `rpc: closing slow client <addr>`.

The wire supports concurrent requests in both directions. Request identifiers MUST route responses to the waiting caller. The transport MUST close when its stream closes. Calls that wait at that time MUST return a closed-pipe error. Cancellation of a caller context MUST remove the pending request.

## Schema

Every response and notification in the Codex subset MUST validate against the vendored schema in `internal/dal/rpc/testdata/codex-schema` (Codex commit `c9b4267c370a755da12ec1c8648c2451163212b7`). Extension methods and notifications under `dalgona/` MUST validate against `internal/dal/rpc/testdata/dalgona-schema`. The server encodes every message with `encoding/json/jsontext` or `encoding/json/v2`.

## Text inputs

[Input text](input-text.md) defines the shared input contract for GUI, TUI, web, and daemon callers.
Its new validation and preservation requirements are planned changes, not claims that the inspected runtime already implements them.

The daemon MUST reject malformed UTF-8 and unpaired JSON surrogate escapes before dispatching a message.
It MUST NOT enable replacement decoding for user input.
A leading byte-order mark outside the JSON value MUST fail parsing.
An encoded `U+FEFF` inside a string is string content and MUST follow the field policy.

The daemon MUST validate every known text parameter before performing an effect for its request.
This includes tool discovery, workspace trust, local-image reads, file writes, credential updates, and journal appends.
Valid JSON that violates shared encoding, size, line-break, control, or offset rules MUST fail with `-32602`.
The error MUST identify the field and the validation reason without echoing its submitted contents.
Its message MUST be `invalid text: <field>: <reason>`.
The ASCII reason MUST be `invalid_encoding`, `text_too_large`, `line_break_not_allowed`, `disallowed_control`, or `invalid_offset`, as applicable.
The field MUST identify the declared parameter location without including a submitted value.
Malformed JSON MUST use `-32700` instead.
Method-specific grammar and schema failures retain their declared errors, including invalid paths, unsupported enums, and rejected provider syntax.
They MUST NOT invent another shared text reason or misclassify a grammar failure as an encoding failure.
Unknown members retain the schema rule above, but their encoding MUST still be valid JSON text.

Accepted free text MUST preserve its decoded Unicode scalar sequence.
The daemon MUST NOT trim, normalize, case-fold, or strip it as a general input cleanup step.
Interactive adapters apply the newline policy in [Input text](input-text.md) once, before submission.
The daemon MUST NOT repeat that conversion.
Existing secret masking and method-specific assembly remain authoritative.
`turn/start` and `turn/steer` join their text input items with one LF, in input order.
A `mention` or `skill` item contributes `@` followed by its path under the existing input conversion.
Preservation applies to each original text item before that explicit assembly.
JSON escape spelling MAY change during transport without changing decoded text.

Decoded UTF-8 byte limits and encoded JSON frame limits are separate checks.
The `dalgona/keywords/match` limit is 64 KiB of decoded UTF-8 bytes.
The answer limit in [Ask tool](ask.md) counts Unicode scalar values as an explicit exception.
A sender MUST check its complete encoded message against each transport budget before writing it.
Websocket frame capacity MUST NOT increase the daemon's 16 MiB line limit.

Paths, identifiers, enums, credentials, and command bytes retain their declared field grammars.
Unicode support MUST NOT turn a path normalization result into authority for a different file.
File data and PTY bytes retain their binary contracts.
This contract MUST NOT rewrite historical journal records or change their hash and checksum inputs.

Existing clients retain the same method names and result schemas.
The strict field checks are a compatibility tightening where an older caller relied on replacement, truncation, or text cleanup.
Implementations MUST expose those failures and preserve editable drafts for correction.
They MUST NOT silently repair a rejected value or retry an effect with modified text.

## Client guarantees

A client MAY validate every message against the schemas. A client MUST ignore an unknown member and an unknown notification method.

- Every response, notification, and server request in the Codex subset MUST validate against `internal/dal/rpc/testdata/codex-schema`.
- Every `dalgona/` message MUST validate against `internal/dal/rpc/testdata/dalgona-schema`.
- Every `thread/status/changed` MUST carry `seq`, an integer of at least 1.
- Per connection and thread, `seq` MUST strictly increase. This holds after a slow-subscriber catch-up.
- A client MUST drop a status whose `seq` is lower than the last status it applied for that thread.
- Notifications of one thread MUST arrive in journal order.
- `item/started` MUST precede the deltas and `item/completed` of the same item.
- `item/fileChange/patchUpdated` for an edit approval MUST precede its `item/fileChange/requestApproval`.
- `warning` MUST precede the sandbox choice request.
- `dalgona/sandbox/chosen` for `Accept` MUST follow the answer to the approval request of the same call.
- A server request MUST close with `serverRequest/resolved` when another client or the server settles it.
- Text, diffs, and outputs MUST be masked before they leave the server. The exceptions are `command/exec` output and `fs/readFile` data.

## Startup

`initialize` MUST be answered without waiting for recovery or for any thread. The result is `{"userAgent":"<product>/<version>","codexHome":"<home>","platformFamily":"unix|windows","platformOs":"macos|linux|windows"}`. `codexHome` is the product home. The server ignores `codexHome` and `CODEX_HOME` from the client. The server accepts the `initialized` notification and does nothing.

`Serve` MUST NOT run host recovery. `dalgona` runs recovery once at daemon start. `dal` recovers a thread when the thread opens.

## Methods

| Method | Result |
|---|---|
| `thread/start {model?, modelProvider?, cwd?, approvalPolicy?, sandbox?, config?}` | `ThreadStartResponse`. The server also sends `thread/started`. A missing `model` takes the model from the current settings. A `model` without `/` is a Codex bare id: the server prefixes it with `modelProvider`, or else with the provider of the current settings model. A missing `approvalPolicy` or `sandbox` takes the current settings value. `config.model_reasoning_effort` sets the thread's default reasoning effort: every turn of the thread that sends no `effort` uses it, and a turn's own `effort` wins. It is journaled with the thread, so `thread/resume` and `thread/fork` keep it, and the response reports it as `reasoningEffort`. A non-empty value outside `low`, `medium`, `high`, `xhigh`, `max` MUST give `-32602` `unsupported model_reasoning_effort <value>`. A value outside the declared levels of the thread's model MUST give `-32602` `unsupported reasoning effort: <value>` before the server creates a thread. A model that declares no level MUST accept the value and send each request without one. |
| `thread/resume {threadId, excludeTurns?}` | `ThreadResumeResponse` with all turns of the active path. The snapshot MUST replay the full active path, including history before a compaction marker. When `excludeTurns` is `true`, `thread.turns` MUST be `[]`. The `tokenUsage` member MUST still hold the totals of the full active path. |
| `thread/list {cursor?, limit?, archived?}` | Threads ordered by `updatedAt`, newest first. `archived` selects archived threads. `limit` defaults to 50. `nextCursor` is the offset of the next page. |
| `thread/read {threadId, includeTurns?}` | `{thread}`. Turns are present only with `includeTurns`. |
| `thread/turns/list {threadId, cursor?, limit?, sortDirection?, itemsView?}` | `{data, nextCursor?, backwardsCursor?}`. `data` holds one page of turns of the active path. [Paged turns](#paged-turns) gives the rules. |
| `thread/name/set {threadId, name}` | `{}`. Subscribers receive `thread/name/updated`. |
| `thread/archive`, `thread/unarchive {threadId}` | `{}` and `{thread}`. Subscribers receive `thread/archived` or `thread/unarchived`. |
| `thread/compact/start {threadId}` | `{}` after the compaction. |
| `thread/revert {threadId, beforeTurnId}` | `{thread}` with no turns. Subscribers receive `thread/reverted`. |
| `thread/fork {threadId, lastTurnId?}` | `ThreadForkResponse`. The server also sends `thread/started` for the fork. |
| `turn/start {threadId, input, effort?, model?, approvalPolicy?, sandboxPolicy?}` | `{turn}` with status `inProgress`. `effort` overrides the reasoning effort for this turn only. An `effort` outside the model's declared levels MUST give `-32602` `unsupported reasoning effort: <value>`. A model that declares no level MUST accept the `effort` and send the request without one. `model`, `approvalPolicy`, and `sandboxPolicy` override this turn and later turns on the active path. A missing value keeps the thread value. A turn `approvalPolicy` MUST NOT lower the thread mode. The server applies the stricter of the thread mode and the mapped turn mode. The order is `yolo` < `auto` < `ask`. A bare `model` id takes the provider of the thread model. The server MUST accept an `image` input item only with a `data:` URL. The server MUST accept a `localImage` input item only with an absolute path. The server MUST ignore the `detail` member of an image input item. The server MUST read a local image file that the daemon user can read. The server MUST return `-32602` for an image that is missing or has the wrong type. |
| `turn/steer {threadId, expectedTurnId?, input}` | `{turnId}`. When `expectedTurnId` is set, it MUST name the running turn. The server MUST check the target atomically with submission. A steer for a turn that is not running MUST fail and MUST NOT start a turn. The server MUST reject such a steer before it resolves tools or asks for workspace trust. The server MUST accept the same image input items as `turn/start`. |
| `turn/interrupt {threadId, turnId}` | `{}`. |
| `review/start {threadId, target, delivery?}` | `{reviewThreadId, turn}`, with `turn.status` `inProgress`. `target.type` MUST be `uncommittedChanges`, `baseBranch` with `branch`, `commit` with `sha` and an optional `title`, or `custom` with `instructions`. Another `target.type` MUST give `-32602` `unsupported review target <type>`. The review runs as an ordinary turn on `threadId` with a fixed review prompt. `delivery` MUST be absent, null, or `inline`. Another `delivery` value, including the empty string, MUST give `-32602` `unsupported delivery <d>`. A branch MUST match `^[A-Za-z0-9._/][A-Za-z0-9._/-]*$`, and a sha MUST be 4 to 64 hex digits. A rejected request MUST NOT start a turn. |
| `model/list` | Models of every configured provider. `id` is `<provider>/<model>`. Each model reports `contextWindow`, its `provider`, and `presets`: the roles `default`, `smol`, and `slow` whose preset names the model, in that order. Each model MUST report `inputModalities`. A model that accepts images MUST report `["text","image"]`. Every other model MUST report `["text"]`. |
| `config/read` | `{config: {model, model_provider, approval_policy, sandbox_mode, model_reasoning_effort}, origins: {}}` from the current settings. |
| `config/value/write {keyPath, value}` | Writes `settings.toml` through `Host.WriteSettings`, which applies the value to the running host at once. Codex keys `approval_policy`, `sandbox_mode`, and `model_reasoning_effort` map to `approval`, `sandbox`, and `reasoning`. Direct keys `model`, `approval`, `sandbox`, and `reasoning` pass through with the same value checks. A `model` value that is not `<provider>/<model>` MUST give `-32602` `model id must be <provider>/<model>`. A `model_reasoning_effort` or `reasoning` value outside the current model's declared levels MUST give `-32602` `unsupported reasoning effort: <value>`. The interactive keys of `dalgona/settings/read` pass through the same checks. Other keys give `-32602`. `version` is the SHA-256 of the written file. |
| `config/batchWrite {edits: [{keyPath, value, mergeStrategy}], filePath?, expectedVersion?, reloadUserConfig?}` | Applies every edit through `Host.WriteSettings`, or none when one edit is invalid. `mergeStrategy` MUST be `replace` or `upsert`, and each edit sets its key. `filePath`, when set, MUST equal the settings path. `expectedVersion`, when set, MUST equal the SHA-256 of the current file, or the server answers `-32600`. `reloadUserConfig` has no effect. Result `{filePath, status: "ok", version}`. |
| `dalgona/settings/read {}` | `{settings: [{key, value, defaultValue, options}]}`. `dalgona` MUST list seven settings in this order: `approval`, `verbosity`, `theme`, `icons`, `statusHide`, `editReview`, `notifications`. `dal` MUST list the same order without `editReview`. Writes use the same keys with `config/batchWrite`. |
| `account/read` | `chatgpt` when a ChatGPT OAuth credential exists, else `apiKey` when any credential exists, else `null`. `requiresOpenaiAuth` is `false`. |
| `account/login/start {type, apiKey?, provider?}` | `apiKey` checks the key with the model list of `provider` and stores the key only when the provider accepts it. A runner service MUST store the key without a check. The key MUST be one line. A rejected key gives `-32602` `<provider> did not accept the API key; it was not saved` and changes no stored credential. `chatgpt` and `chatgptDeviceCode` run the ChatGPT OAuth flow and send `account/login/completed`. Other OAuth providers MUST log in through the terminal (`<product> login <provider>`). |
| `account/login/cancel {loginId}` | `{status: canceled|notFound}`. |
| `account/logout {provider?}` | `{}`. Without `provider`, exactly one credential MUST exist. |
| `dalgona/account/providers/list {}` | `{providers: [{id, login, signedIn}]}`, sorted by `id`. `login` is `apiKey` for the API-key providers and the runner services. It is `browser` for `chatgpt` and `hybrid` for a provider in both lists. It is `terminal` for the other OAuth providers. |
| `skills/list` | One entry with the host skills. `dal` returns `{"data":[]}`. |
| `mcpServerStatus/list {cursor?, limit?, serverName?, threadId?}` | `{data: [{name, authStatus, tools, resources, resourceTemplates, runtimeStatus, toolsError?}], nextCursor?}`. `limit` defaults to 100 and MUST be at most 1000. `threadId` selects the workspace of that thread. Tool names drop the `<server>__` prefix. |
| `command/exec {command, cwd?, env?, timeoutMs?, disableTimeout?, outputBytesCap?, disableOutputCap?, processId?, streamStdoutStderr?, sandboxPolicy?}` | `{exitCode, stdout, stderr}`, where `stderr` is empty and stdout and stderr arrive merged. A `command/exec` call has no thread. The server MUST run the command under the host's configured sandbox setting and MUST NOT ask for approval. The server MUST ignore a `sandboxPolicy` member except `externalSandbox`, so a client cannot turn the sandbox off for one command. `cwd` defaults to the configured working directory. `timeoutMs` defaults to 600000, and a timeout gives exit code 124. A negative `timeoutMs` MUST give `-32602` `timeoutMs must not be negative`. `outputBytesCap` defaults to 1 MiB. An `outputBytesCap` outside 1 to 67108864 MUST give `-32602` `outputBytesCap must be from 1 to 67108864`. At most 16 commands MAY run on one connection, and a command beyond that MUST give `-32602` `too many running commands`. With `streamStdoutStderr`, the server MUST send `command/exec/outputDelta` notifications for the process before it answers, and `processId` MUST be set. `tty`, `streamStdin` and `size` are not supported. |
| `command/exec/terminate {processId}` | `{}`. The server MUST send a termination signal to the running process with that `processId`, and it MUST NOT wait for the process to end. An unknown process gives `-32602` `unknown process <id>`. |
| `dalgona/keywords/list` | `[{word, hue:[a,b]}]` for enabled keywords. |
| `dalgona/thread/tree {threadId}` | `{turns:[{turnId, parentTurnId, preview, active}]}`. `parentTurnId` is `null` for a first turn. |
| `dalgona/thread/leaf/set {threadId, turnId}` | `{}`. |
| `dalgona/thread/search {query, limit?, cursor?, archived?, cwd?}` | `{data:[{threadId, name, preview, cwd, updatedAt, seq, kind, snippet, matches}], nextCursor?}`. The server searches what was said in threads. `limit` defaults to 20 and MUST be at most 100. `cursor` is the offset as a decimal string. A missing or empty `query` MUST give `-32602` `query is required`. A term shorter than 3 characters MUST give `-32602` `query must hold at least 3 characters`. A mirror error MUST give `-32603` with the cause. See Thread search. |
| `dalgona/advisor/status {threadId}` | `[{name, model, status: idle|reviewing|halted|disabled, notes, usage:{inputTokens, outputTokens}}]`. |
| `dalgona/advisor/set {threadId, enabled}`, `dalgona/advisor/reset {threadId}` | `{}`. |
| `dalgona/item/output/read {threadId, itemId, offset?}` | `{text, offset, next, total}`. `next` equals `total` at the end of the output. A call without a spill MUST give `-32602` `call <id> has no spilled output`. |
| `dalgona/keywords/match {threadId?, text, kind?}` | `{matches:[{word, byteRange:{start, end}, hue, effort?, summary}]}`, sorted by start. `kind` is `prompt` (the default) or `steer`. Text over 64 KiB MUST give `-32602` `invalid text: text: text_too_large`. |
| `fs/readFile {path}`, `fs/writeFile {path, dataBase64}`, `fs/createDirectory {path, recursive?}` | `{dataBase64}`, `{}`, `{}`. See File methods. |
| `fs/getMetadata {path}`, `fs/readDirectory {path}` | `{isDirectory, isFile, isSymlink, createdAtMs, modifiedAtMs}`, `{entries: [{fileName, isDirectory, isFile}]}`. See File methods. |
| `fs/remove {path, recursive?, force?}`, `fs/copy {sourcePath, destinationPath, recursive?}` | `{}`. See File methods. |
| `fs/watch {watchId, path}`, `fs/unwatch {watchId}` | `{path}`, `{}`. The server sends `fs/changed` notifications for a watch. See File methods. |
| `fuzzyFileSearch {query, roots, cancellationToken?}` | `{files: [{root, path, file_name, match_type, score, indices}]}`, at most 50 results. See Fuzzy file search in `search.md`. |
| `dalgona/retry/wait {threadId, turnId, remainingMs}` | Notification, not a method. Sent once a second during an attended rate-limit wait, and once with `remainingMs` 0 when the wait ends. Not journaled. |
| `dalgona/daemon/identity` | `{build, pid, version, startedAt}`. The daemon answers it before `initialize`. `build` is the first 16 hex digits of the SHA-256 of the daemon executable. |
| `dalgona/daemon/retire` | `{}`. The daemon cancels its run context and stops as on `SIGTERM`. The client counts the reply and the end of the stream as success. |

`thread/revert`, `thread/fork`, `turn/start`, and `dalgona/thread/leaf/set` require an idle thread. A running thread MUST give `-32600` `thread <id> is running`.

A client MUST handle `account/login/completed` as follows. On `success` true, it MUST close the login panel, show `Signed in to <provider>`, and request `model/list` again. On `success` false, it MUST keep the panel open and show `error` beside the key field. A client MUST NOT promise that a turn resumes after a login.

A product MAY add methods. Built-in methods take precedence over added methods. A product MAY run an archive hook on an idle thread. The hook MUST run after `Host.Archive` journals `thread.archived`. If the hook fails, the server MUST unarchive the thread. The server MUST return error `-32603` with the message `archive hook: <cause>`.

### Paged turns

`thread/turns/list` pages the turns of the active path. The server builds a page from the events of the turns on that page only.

`limit` defaults to 20. A `limit` above 100 means 100. A negative `limit` MUST give `-32602` `invalid limit <n>`.

`sortDirection` defaults to `desc`, which lists the newest turn first. `asc` lists the oldest turn first. Another value MUST give `-32602` `unsupported sortDirection <value>`.

`itemsView` `full`, `summary`, and an absent value send every item of each turn. The server treats `summary` as `full`. `notLoaded` sends `items` as `[]` and loads no blob. Another value MUST give `-32602` `unsupported itemsView <value>`.

A cursor is `a` or `b` followed by `<seq>`. `<seq>` is the decimal journal sequence of a `turn.started` record on the active path. `a<seq>` starts after that turn in the sort direction. `b<seq>` starts at that turn.

`nextCursor` is `a<seq>` of the last turn of the page. The server omits it when no turn follows. `backwardsCursor` is `b<seq>` of the first turn of the page. The server omits it when the page is empty. A client passes `backwardsCursor` with the opposite `sortDirection` to read that turn again and the turns after it.

A malformed cursor MUST give `-32602` `invalid cursor <cursor>`. A cursor whose sequence names no `turn.started` record on the active path MUST give `-32602` `cursor <cursor> is not on the active path`.

An unknown thread MUST give `-32602` `unknown thread <id>`. A running thread MAY be listed. Its running turn has status `inProgress`. A thread with no turn gives `data` as `[]` and no cursor.

The server still reads the whole journal of the thread for each call. A page limits only the transfer. With `excludeTurns`, the server loads no blob for a turn that has ended.

### Thread search

The server matches every term of the masked query, so the terms join with AND. It returns one row per thread, ranked by the best matching row. The server drops hits whose archived state differs from `archived`, and hits whose `cwd` differs when `cwd` is set.

The server MUST NOT return a thread whose `session.created` event sets `ephemeral` to `true`. The journal search omits that thread before the server applies its page limit.

The index holds the inline `text` of `user.message` and `assistant.message` events and the `name` of `thread.named` events. `kind` is `user`, `assistant` or `title`. `matches` is the number of query terms in the hit row. Text stored in a blob is never indexed.

The server masks the query before it queries the index, and it masks each snippet again before it answers. A snippet holds at most 160 runes around the first term. Newlines become spaces, and a cut end gets `…`.

### Input images

The server MUST accept at most 10 images in one request.
The server MUST accept an image of at most 5242880 bytes.
The server MUST accept at most 10485760 bytes of image data in one request.
The server MUST accept only PNG, JPEG and GIF image data.
The server MUST detect the image type from the first bytes of the data.

## MCP authentication status

`authStatus` is `unsupported` for stdio servers and HTTP servers without authentication.
It is `bearerToken` when the configured environment variable has a value.
It is `oAuth` when a resource-bound OAuth credential exists.
It is `notLoggedIn` when the environment variable is empty or the OAuth credential is absent or invalid.
The server MUST NOT emit `unknown`.
OAuth without a credential MUST make no connection request.
It reports `runtimeStatus` as `authenticationRequired` with empty tools.
Its `toolsError` is `login required: run dalgona mcp login <name>`.

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

`tokenUsage.estimatedContextTokens` MUST carry the context token estimate that the newest journal compaction recorded. The server MUST omit it when no compaction has run, and after the next assistant message reports provider usage. A known estimate of `0` MUST be sent as `0`. Absence means no estimate, not an empty context. The estimate comes from the journal. It is not a provider count, and it MUST NOT change `last`, `total`, or the cost members. These rules apply to `thread/tokenUsage/updated` and to the `tokenUsage` of `thread/start`, `thread/resume`, and `thread/fork`.

`thread/resume` MUST include `turnEffort` only when the active turn has a resolved request effort. Its value MUST match the newest `turn.effort` of that turn. A resolved empty string MUST be present as `""`. An idle thread or an unresolved active turn MUST omit `turnEffort`. Replay MUST rebuild this state without sending a live effort notification. The `reasoningEffort` member remains the thread default.

`tokenUsage.costUsdMicros` MUST be an integer that counts millionths of a US dollar. The server MUST omit it until one assistant message has a price. `tokenUsage.costPartial` MUST be `true` when a priced message and a message on a model with no price both exist. The server MUST omit `costPartial` otherwise. These rules apply to `thread/tokenUsage/updated` and to the `tokenUsage` of `thread/start`, `thread/resume`, and `thread/fork`. The price rules are in [providers.md](providers.md#cost). `costUsdMicros` and the `last` and `total` counts come from provider usage reports. A compaction MUST NOT change them.

## Server requests

The server sends approval requests only for threads in approval mode `ask`, plus one workspace trust prompt per untrusted workspace. An exec call uses `item/commandExecution/requestApproval`. An edit call uses `item/fileChange/requestApproval`. A workspace trust prompt reuses `item/commandExecution/requestApproval`. It sets `cwd` to the workspace path. It sets `command` to the server commands and env, joined by newlines. It sets `reason` to the full workspace prompt. The request `id` is the approval request id of the journal. `itemId` is the tool call id, empty for workspace trust.

The server MUST send `item/fileChange/patchUpdated` for the item before an edit approval request. Its `changes` carry the unified diff and the line counts of each file that the call would change. A file over the diff limit has `diffOmitted` set and no counts.

Decisions `accept`, `acceptForSession`, `decline`, and `cancel` map to the host replies. The decision `approved` maps to accept, `approved_for_session` maps to acceptForSession, and `abort` maps to cancel. An object decision MUST count as `accept`. Other values MUST count as `decline`. When the approval resolves, every subscriber receives `serverRequest/resolved {threadId, requestId, resolution}` and the server cancels its pending request. A resolved approval MUST report `answered` for an accept and `declined` for a decline. A cancel by the user MUST report `cancelled`, and a cancel that an interrupt caused MUST report `interrupted`. Any other close MUST report `declined`. When a client fails to answer and it is the last subscriber, the server MUST decline. A client MUST handle each server request on its own task. An open prompt MUST NOT block notification delivery. A client MUST cancel that task when the matching `serverRequest/resolved` arrives.

The server sends `item/tool/requestUserInput` for an `ask` call. The request `id` is a question request id that the host generates, and the host does not journal it. `itemId` is the call id. Question ids are `q1` to `q4` by position. Each question MAY carry the extensions `multiSelect` and `preview`. A client that ignores them still works. The server MUST replay an open question to a window that subscribes later, under the same request id. The server MUST count a resubscribed window as refused when that window refused the question before the host closed it as slow.

A client MUST answer with `answers` that maps each question id to an object containing an `answers` list of strings.
For example, `{"answers":{"q1":{"answers":["chosen value"]}}}` answers the first question.
An answer equal to an exact offered option label is that option. Any other string is typed text.

An otherwise structurally valid answer result MUST satisfy [Ask tool](ask.md) and [Input text](input-text.md) before resolving the host question.
The transport MUST validate a typed answer response before removing its pending response waiter.
A structurally valid response with invalid answer text MUST leave that waiter and the host question pending.
The original request id, deadline, and subscriber set MUST remain unchanged.
The server MUST NOT send a JSON-RPC error response to an answer response.
A JSON result with the wrong answer shape retains the existing unreadable-answer failure outcome.
The new correction path MUST NOT change that failure or the existing handling of client error responses.

Instead, it MUST send `warning` with `threadId`, `message`, `requestId`, `reason`, and `field` to the answering connection.
The last three members are optional extensions for other warning uses.
The message and reason MUST follow the text error contract above.
The `requestId` MUST name the same pending question.
The field MUST identify the invalid answer, such as `answers.q1.answers[0]`, without exposing its text.
This warning MUST NOT carry `serverRequest/resolved` or create a journal event.

First-party clients MUST retain the draft and question card until `serverRequest/resolved`, including after sending an answer.
On a correction warning, they MUST show the field error and permit a corrected response with the same request id.
Their typed question lifecycle MUST retain the response sender after the first send and support later corrections while the question remains pending.
The current one-response request callback path MUST migrate to that lifecycle for user-input questions.
Each later response MUST follow a matching correction warning and use the original request id.
The client MUST stop accepting corrections when `serverRequest/resolved` closes that lifecycle.
A warning for an already resolved request MUST NOT reopen its card.
Concurrent valid answers and the original timeout retain their existing first-winner rules.
An older client MAY lack a correction interface if it discarded its draft or response sender after replying.
Its text-validation failure MUST NOT resolve the host question.
The existing last-subscriber, interruption, and timeout rules remain unchanged.
Reconnecting or reopening MUST NOT be promised as recovery, because losing the last subscriber can resolve the question.
The server MUST NOT accept invalid answer text to preserve that older behavior.

These rules require typed response validation before the current response dispatcher consumes its waiter.
Retaining the server waiter alone does not provide the required client correction lifecycle.
They are planned transport work, not a new request method or a new question id scheme.

A client MUST answer `{"answers": {}}` to dismiss the questions. Any other error response MUST mean that the window cannot answer.

The server MUST decline a question with the no-client result when every attached window cannot answer. The first valid answer MUST decide the question. Every subscriber receives `serverRequest/resolved` with a `resolution` of `answered`, `cancelled`, `interrupted`, `timedOut` or `declined`. A question that no window answers within 5 minutes MUST report `timedOut`.

The sandbox choice uses `item/tool/requestUserInput` with the header `Sandbox`, the options `Accept` and `Dangerously accept`, and no Other row. The server sends a `warning` notification before the question. A cancelled question MUST decline the command, and the next command MUST ask again. After a choice is journaled, the server MUST send `dalgona/sandbox/chosen` with `threadId`, `cause`, and `choice`. `choice` is `accept` or `dangerouslyAccept`. For `Accept`, the notification MUST follow the answer to the approval request of the same call. For `Dangerously accept`, the notification MUST follow the answer to the sandbox question.

## Notifications and items

`waitingOnUserInput` is an active flag that the server sets while a question is open. It carries no journal event.

Each connection subscribes to every thread that it starts, resumes, or changes. The server translates journal events and stream deltas:

`thread/status/changed` carries the extension member `seq`: the journal sequence number of the event that changed the status. A status without a journal event MUST carry the larger of the latest applied journal `seq` and the last sent `seq` plus one. The client rule is defined in [Client guarantees](#client-guarantees).

Turn objects carry `startedAt` and `completedAt` as Unix seconds, and `durationMs` in milliseconds on `turn/completed`. A `commandExecution`, `mcpToolCall`, or `dynamicToolCall` item MUST carry `durationMs` when the journal holds both of its times. The server computes each duration from the journal times `started_at` and `completed_at`, so a replay shows the same values as the live stream.

| Source | Notification |
|---|---|
| `turn.started` | `turn/started`, `thread/status/changed` active |
| `turn.completed` | `error` when the turn failed, `turn/completed`, `thread/status/changed` idle |
| `turn.judged` | `dalgona/turn/judged {threadId, turnId, verdict, summary, paths, by, fallback?, judges?}`, live only. `by` names the answering model. `fallback` names the primary model and its failure. |
| `turn.effort` | `dalgona/turn/effort {threadId, turnId, effort}`, live only. All three members MUST be present. Empty `effort` is authoritative. |
| `user.message` | `item/started` and `item/completed` with a `userMessage`. The server MUST echo each image of a user message as an `image` input item. The echoed image item MUST NOT carry image bytes. The echoed image item MUST name the blob with the `dalgona-blob:` prefix. |
| text delta | `item/started` once, then `item/agentMessage/delta` |
| reasoning delta | `item/started` once, then `item/reasoning/textDelta` for item `<item>-reasoning` |
| `assistant.message` | `item/completed` for the `agentMessage` and the `reasoning` item, `thread/tokenUsage/updated` |
| `approval.requested` | `item/started` for the call when it is not started and the journal knows its tool name, `thread/status/changed` with `waitingOnApproval`. A nested call whose tool name is not journaled yet MUST get no item here. |
| `approval.resolved` | `serverRequest/resolved` |
| `tool.started` | `item/started` once per call. This opens a nested call whose name was unknown at approval time. |
| `tool.completed` | `item/completed`. For the `todo` tool, also `turn/plan/updated` |
| `tool.judged` | `dalgona/tool/judged {threadId, turnId, itemId, verdict, summary, paths, by, fallback?, judges?}`, live only. `by` names the answering model. `fallback` names the primary model and its failure. `judges` holds `{part, by, fallback?}` for each part of a split unit. |
| `compaction` | `contextCompaction` item, and `thread/tokenUsage/updated` with `estimatedContextTokens` |
| `model.retry` | `error` with `willRetry: true` |
| `thread.named` | `thread/name/updated` |
| `thread.archived` | `thread/archived` or `thread/unarchived` |
| `branch` | `thread/reverted` |
| `child.spawned` | `dalgona/thread/childSpawned {threadId, childId, cwd}` |
| `advisor.note` | a completed `dynamicToolCall` item with tool `advisor`, and `dalgona/advisor/note {threadId, advisor, severity, note, delivery}` |

The effort notification MUST follow successful request resolution, not `turn/started`. The server MUST ignore an effort event for a different turn. The first resolution MUST notify, including an empty effort. Unchanged resolutions in later rounds MUST NOT notify again. Turn start and completion MUST clear the resolved effort.

The tool class sets the item kind. Class `exec` gives `commandExecution`. Class `edit` gives `fileChange` with one change per file. Class `remote` gives `mcpToolCall` with the tool name `<server>__<tool>` after the MCP name rule. Any other tool gives `dynamicToolCall`. A call that completes without a start, with a `decline` or `cancel` decision, has status `declined` for `commandExecution` and `fileChange`. For other kinds, that call has status `failed`. Its output is the decline reason.

A tool spec carries a `Serial` flag. A call to a tool with `Serial` set MUST run alone, never in a batch with another call. The tools `todo`, `ask`, and `yield` set `Serial`. A tool that leaves `Serial` unset MAY run in a batch with adjacent read-class calls.

A file change carries a unified diff of its journaled pre-image and post-image, with three lines of context. `additions` and `deletions` count the added and removed lines. A file without a pre-image is `add`. A file without a post-image is `delete`.

A file change MUST set `diffOmitted` to `true` when either image is over 1 MiB. It MUST also do so when an image has a digest but no blob, or when its blob cannot be loaded. An update record written before post blobs existed has a post digest and no post blob. Such a change MUST have an empty `diff` and no counts.

`commandExecution`, `mcpToolCall`, and `dynamicToolCall` items MUST carry `outputSpill` when their output is spilled. `outputSpill` MUST hold `path`, `bytes`, and `sha256`, and the client MUST read the rest of the output with `dalgona/item/output/read`. A page MUST hold at most 1 MiB of source bytes, and it MUST end at a line break when one lies in its window. Without a line break, a page MUST end at a UTF-8 rune start, or at the byte limit if none exists.

When a subscription closes because the subscriber was slow, the server MUST subscribe again and replay the missed events from the journal in sequence order. An event with a sequence number at or below the last delivered one MUST NOT be sent twice.

A `userMessage` item MUST carry `keywords` when its prompt fired notices. Each entry MUST hold the match that `dalgona/keywords/match` returns for that prompt, with its `byteRange` into the item text. The server MUST match keywords without checking required tools until the thread has resolved its tools.

## Clients

`dal` and `dalgona` share one command line (`internal/dal/app` `RunCLI`):

- `-p <prompt>` runs one turn and exits.
  Without `--json`, it prints the agent messages of the turn.
  With `--json`, it writes completed items and a final result as JSONL to stdout.
  Each line carries `schemaVersion:1`, `type`, and `thread`.
  Item lines also carry `turn` and the original masked `item` object.
  Result lines also carry `status` and `stop_reason`.
  `stop_reason` is `stop` for a completed turn.
  It is the turn error for a failed turn.
  It is `interrupted` for an interrupted turn.
  The exit code is 0 only for a completed turn.
  An output failure stops further output and submission.
- Headless runs read non-terminal stdin to EOF before submission.
  Stdin has no read deadline and accepts at most 1048576 bytes.
  Use `</dev/null` or `ssh -n` to avoid unintended stdin input.
  The client MUST reject malformed UTF-8 before submission, replacing the previous U+FFFD repair behavior.
  This is an explicit compatibility tightening under [Input text](input-text.md).
  The existing headless adapter removes trailing CR and LF characters as a documented CLI convenience.
  That transformation MUST NOT apply to GUI, TUI, web, or direct daemon text inputs.
  Internal line endings and other accepted scalar values MUST remain unchanged.
  Nonempty stdin precedes a nonempty prompt with two newlines between them.
  Empty stdin leaves the prompt unchanged.
  Stdin alone supplies an empty prompt.
- `--resume <id>` and `-c` select a thread.
  With no prompt or stdin, headless mode waits for recovery without starting another turn.
  Without either selector, `-p` requires a prompt or piped input.
- `-m`, `--approval auto|ask|yolo`, `--sandbox workspace-write|off` apply to a new thread. `-C <dir>` changes the working directory first.
- In `-p` mode the client declines every approval request and refuses every question, so the model receives `declined: no client could answer this question`.
- `login [provider] [--device-auth]`, `logout <provider>`, and `models` complete without the TUI. `login` without a provider lists the providers.

`app.TUIOptions.OnStatus` MUST receive `app.Status` values that `cmd/internal/tuiwire` converts from `tui.Status`. `internal/dal/app` MUST NOT import `internal/dal/tui`, so command entry points set `CLIEnv.TUI`.
