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

Every response and notification in the Codex subset MUST validate against the vendored schema in `internal/rpc/testdata/codex-schema` (Codex commit `c9b4267c370a755da12ec1c8648c2451163212b7`). Extension methods and notifications under `dalgona/` have no Codex schema. The server encodes every message with `encoding/json/jsontext` or `encoding/json/v2`.

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
| `turn/start {threadId, input, effort?, model?, approvalPolicy?, sandboxPolicy?}` | `{turn}` with status `inProgress`. `effort` overrides the reasoning effort for this turn only. `model`, `approvalPolicy`, and `sandboxPolicy` override this turn and later turns on the active path; a missing value keeps the thread value. A turn `approvalPolicy` MUST NOT lower the thread mode: the server applies the stricter of the thread mode and the mapped turn mode, in the order `yolo` < `auto` < `ask`. A bare `model` id takes the provider of the thread model. |
| `turn/steer {threadId, expectedTurnId?, input}` | `{turnId}`. When `expectedTurnId` is set, it MUST name the running turn. The server MUST check the target atomically with submission. A steer for a turn that is not running MUST fail and MUST NOT start a turn. The server MUST reject such a steer before it resolves tools or asks for workspace trust. |
| `turn/interrupt {threadId, turnId}` | `{}`. |
| `model/list` | Models of every configured provider. `id` is `<provider>/<model>`. |
| `config/read` | `{config: {model, model_provider, approval_policy, sandbox_mode, model_reasoning_effort}, origins: {}}` from the current settings. |
| `config/value/write {keyPath, value}` | Writes `settings.toml` through `config.WriteSetting`. Codex keys `approval_policy`, `sandbox_mode`, and `model_reasoning_effort` map to `approval`, `sandbox`, and `reasoning`. Direct keys `model`, `approval`, `sandbox`, and `reasoning` pass through with the same value checks. Other keys give `-32602`. `version` is the SHA-256 of the written file. |
| `account/read` | `chatgpt` when a ChatGPT OAuth credential exists, else `apiKey` when any credential exists, else `null`. `requiresOpenaiAuth` is `false`. |
| `account/login/start {type, apiKey?, provider?}` | `apiKey` stores the key for `provider` (default `openai`). `chatgpt` and `chatgptDeviceCode` run the ChatGPT OAuth flow and send `account/login/completed`. Other OAuth providers MUST log in through the terminal (`dal login <provider>`). |
| `account/login/cancel {loginId}` | `{status: canceled|notFound}`. |
| `account/logout {provider?}` | `{}`. Without `provider`, exactly one credential MUST exist. |
| `skills/list` | One entry with the host skills. `dal` returns `{"data":[]}`. |
| `dalgona/keywords/list` | `[{word, hue:[a,b]}]` for enabled keywords. |
| `dalgona/thread/tree {threadId}` | `{turns:[{turnId, parentTurnId, preview, active}]}`. `parentTurnId` is `null` for a first turn. |
| `dalgona/thread/leaf/set {threadId, turnId}` | `{}`. |
| `dalgona/advisor/status {threadId}` | `[{name, model, status: idle|reviewing|halted|disabled, notes, usage:{inputTokens, outputTokens}}]`. |
| `dalgona/advisor/set {threadId, enabled}`, `dalgona/advisor/reset {threadId}` | `{}`. |

`thread/revert`, `thread/fork`, `turn/start`, and `dalgona/thread/leaf/set` require an idle thread. A running thread MUST give `-32600` `thread <id> is running`.

A product MAY add methods. Built-in methods take precedence over added methods. A product MAY run a precondition before `thread/archive` on an idle thread. If the precondition fails, the thread MUST stay unarchived. The server MUST return error `-32603` with the message `archive hook: <cause>`.

## Policy mapping

`approvalPolicy`: `never` maps to `yolo`. `on-request` and `on-failure` map to `auto`. `untrusted` and any `granular` object map to `ask`. Another object MUST give `-32602`. `sandbox`: `danger-full-access` maps to `off`. `workspace-write` and `read-only` map to `workspace-write`. `sandboxPolicy.type`: `dangerFullAccess` maps to `off`. `workspaceWrite` and `readOnly` map to `workspace-write`. `externalSandbox` MUST give `-32602` `unsupported sandboxPolicy externalSandbox`. Responses report `yolo` as `never`, `auto` as `on-request`, and `ask` as `untrusted`. Responses report `off` as `dangerFullAccess`, else `workspaceWrite`. The `model` of every thread object MUST be the effective model of the active path. `thread/start`, `thread/resume`, and `thread/fork` MUST also report the effective approval and sandbox. They MUST NOT report the `session.created` values. The effective sandbox is `off` when the configured policy is not `off` and the runner reports no harness sandbox. The server gets this value from the agent layer.

## Server requests

The server sends approval requests only for threads in approval mode `ask`, plus one workspace trust prompt per untrusted workspace. An exec call uses `item/commandExecution/requestApproval`. An edit call uses `item/fileChange/requestApproval`. A workspace trust prompt reuses `item/commandExecution/requestApproval`. It sets `cwd` to the workspace path. It sets `command` to the server commands and env, joined by newlines. It sets `reason` to the full workspace prompt. The request `id` is the approval request id of the journal. `itemId` is the tool call id, empty for workspace trust.

Decisions `accept`, `acceptForSession`, `decline`, and `cancel` map to the host replies. The decision `approved` maps to accept, `approved_for_session` maps to acceptForSession, and `abort` maps to cancel. An object decision MUST count as `accept`. Other values MUST count as `decline`. When the approval resolves, every subscriber receives `serverRequest/resolved {threadId, requestId}` and the server cancels its pending request. When a client fails to answer and it is the last subscriber, the server MUST decline. A client MUST handle each server request on its own task. An open prompt MUST NOT block notification delivery. A client MUST cancel that task when the matching `serverRequest/resolved` arrives.

## Notifications and items

Each connection subscribes to every thread that it starts, resumes, or changes. The server translates journal events and stream deltas:

`thread/status/changed` carries the extension member `seq`: the journal sequence number of the event that changed the status. A client MUST ignore a status with a lower `seq` than one it already applied.

| Source | Notification |
|---|---|
| `turn.started` | `turn/started`, `thread/status/changed` active |
| `turn.completed` | `error` when the turn failed, `turn/completed`, `thread/status/changed` idle |
| `user.message` | `item/started` and `item/completed` with a `userMessage` |
| text delta | `item/started` once, then `item/agentMessage/delta` |
| reasoning delta | `item/started` once, then `item/reasoning/textDelta` for item `<item>-reasoning` |
| `assistant.message` | `item/completed` for the `agentMessage` and the `reasoning` item, `thread/tokenUsage/updated` |
| `approval.requested` | `item/started` for the call when not started, `thread/status/changed` with `waitingOnApproval` |
| `approval.resolved` | `serverRequest/resolved` |
| `tool.started` | `item/started` |
| `tool.completed` | `item/completed`; for the `todo` tool also `turn/plan/updated` |
| `compaction` | `contextCompaction` item |
| `model.retry` | `error` with `willRetry: true` |
| `thread.named` | `thread/name/updated` |
| `thread.archived` | `thread/archived` or `thread/unarchived` |
| `branch` | `thread/reverted` |
| `child.spawned` | `dalgona/thread/childSpawned {threadId, childId, cwd}` |
| `advisor.note` | a completed `dynamicToolCall` item with tool `advisor`, and `dalgona/advisor/note {threadId, advisor, severity, note, delivery}` |

The tool class sets the item kind. Class `exec` gives `commandExecution`. Class `edit` gives `fileChange` with one change per file. Class `remote` gives `mcpToolCall` with the tool name `<server>__<tool>` after the MCP name rule. Any other tool gives `dynamicToolCall`. A call that completes without a start, with a `decline` or `cancel` decision, has status `declined` for `commandExecution` and `fileChange`. For other kinds, that call has status `failed`. Its output is the decline reason.

A file change carries the per-file section of the patch text. A file without a pre-image is `add`. A file without a post-image is `delete`.

When a subscription closes because the subscriber was slow, the server MUST subscribe again and replay the missed events from the journal in sequence order. An event with a sequence number at or below the last delivered one MUST NOT be sent twice.

## Clients

`dal` and `dalgona` share one command line (`internal/app` `RunCLI`):

- `-p <prompt>` runs one turn and exits. Without `--json` it prints the agent messages of the turn. With `--json` it prints `{"thread":"<id>","status":"completed|interrupted|failed","stop_reason":"..."}`. `stop_reason` is `stop` for a completed turn, the turn error for a failed turn, and `interrupted` for an interrupted turn. The exit code is 0 only for a completed turn.
- `--resume <id>` and `-c` (latest thread) select a thread. With `-p ""`, the command waits for a recovered turn and does not start a new turn.
- `-m`, `--approval auto|ask|yolo`, `--sandbox workspace-write|off` apply to a new thread. `-C <dir>` changes the working directory first.
- In `-p` mode the client declines every approval request.
- `login [provider]`, `logout <provider>`, and `models` complete without the TUI. `login` without a provider lists the providers.
