# Agent events

Each journal event MUST be one JSON object with a string `type`. The encoder MUST NOT append a newline. The decoder MUST reject malformed JSON, trailing data, invalid field types, unknown event types, and validation failures.

Identifiers MUST NOT be empty. Enumerated fields MUST use the values in this specification. Token counts, repair counts, effect indexes, delays, and blob sizes MUST NOT be negative. SHA-256 fields MUST contain 64 lowercase hexadecimal characters. The encoder MUST write strings as JSON strings and MUST replace invalid UTF-8 with U+FFFD.

## Payload references

An inline text or raw JSON payload MUST NOT exceed 64 KiB. A larger payload MUST use a typed blob reference in the same JSON field:

```json
{"blob":"<64 lowercase hex>","bytes":123456}
```

A field MUST NOT contain both an inline value and a blob reference. `tool.completed.output` MUST use `spill` instead of a blob reference. Its spill object MUST contain `path`, `bytes`, and `sha256`. With a spill, the output is a projection: at most 64 KiB of content plus the truncation notice, and in total at most 68 KiB.

## Event records

|Type|Required fields|Optional fields and constraints|
|---|---|---|
|`session.created`|`id`, `cwd`, `model`, `approval`, `sandbox`, `runner`|`parent`, `fork_of`, `effort`, `instructions_blob`, `advisor`, `created_at`. Approval is `auto`, `yolo`, or `ask`. Sandbox is `workspace-write` or `off`. `effort` is `low`, `medium`, `high`, `xhigh`, or `max` and is the thread default for turns without their own effort. `advisor` names an advisor child thread and MUST come with `parent`. `created_at` is an RFC 3339 timestamp that the encoder writes in UTC|
|`runner.bound`|`runner`, `service`, `box_id`, `baseline`|None|
|`turn.started`|`turn_id`|`started_at`, an RFC 3339 UTC timestamp that the server stamps at append|
|`turn.completed`|`turn_id`, `status`|`error`. Status is `completed`, `interrupted`, or `failed`. `completed_at`, an RFC 3339 UTC timestamp that the server stamps at append|
|`user.message`|`item_id`, `turn_id`, `text`, `kind`|`notices`, `mentions`, `images`, `effort`, `model`, `approval`, `sandbox`, `verbatim`. Kind is `prompt`, `steer`, or `followup`. Each notice has `word` and `text`. Each mention has `path` and `text`. Each image has `mime`, optional `path`, and `image`, a blob reference. Effort is `low`, `medium`, `high`, `xhigh`, or `max` and overrides config for its turn. Approval and sandbox use the `session.created` values. A `verbatim` message MUST NOT have notices or mentions|
|`assistant.message`|`item_id`, `turn_id`, `text`, `reasoning`, `tool_calls`, `usage`, `stop_reason`|`context_seq`. Usage contains `input`, `output`, `cache_read`, and `cache_write`. Each tool call has `id`, `name`, `args`, `freeform`, and `resolved`. Each tool call MAY have a `signature`, the provider thought-signature replayed to the next request. Each tool call MAY have `redacted_thinking`, opaque provider strings replayed unchanged. `context_seq` is the journal head when the request was built. `context_window` is the model context window in tokens, and it is omitted when unknown|
|`tool.started`|`call_id`, `turn_id`, `tool`, `args`, `class`, `repairs`|`files`. Class is `read`, `edit`, `exec`, `agent`, `script`, or `remote`. Each file has `path`, `pre_sha`, `post_sha`, and optional `pre_blob`. Each file MAY have `post_blob`, the masked post-image of the file. `started_at`, an RFC 3339 UTC timestamp that the server stamps at append|
|`tool.completed`|`call_id`, `ok`, `output`, `recovered`|`spill`, `exit_code`, `meta`. Recovered is `none`, `replayed`, `interrupted`, or `skipped`. `completed_at`, an RFC 3339 UTC timestamp that the server stamps at append|
|`model.retry`|`turn_id`, `attempt`, `verdict`, `status`, `delay_ms`|Attempt MUST be positive. Verdict is `retry` or `overflow`|
|`approval.requested`|`request_id`, `call_id`, `summary`|`class`. Class uses the `tool.started` values. An exec approval MAY carry `grant_prefix`, the argv prefix folded into a thread grant when `args` is spilled to a blob|
|`approval.resolved`|`request_id`, `decision`, `by`, `reason`|`scores`. Decision is `accept`, `accept_session`, `decline`, or `cancel`. Each score MUST be between zero and one|
|`compaction`|`first_kept_seq`, `summary`, `method`, `tokens_before`, `tokens_after`|`first_kept_seq` MUST be positive. Method is `auto`, `manual`, or `overflow`|
|`context.pruned`|`turn_id`, `call_ids`|`call_ids` MUST contain at least one non-empty identifier|
|`child.spawned`|`call_id`, `child_id`, `runner`|None|
|`branch`|`parent_seq`|Zero selects the empty root path|
|`script.started`|`run_id`, `source_blob`, `entry`, `args`, `seed`|None|
|`script.effect`|`run_id`, `n`, `op`, `args_sha`, and `result` or `error`|Effect indexes are assigned at submission, so they MAY arrive out of order. An index MUST NOT repeat. `error` records a failed effect, such as a declined script tool call, and excludes `result`. A denial MUST set `denied` to `true` alongside `error`, and the encoder MUST omit `denied` otherwise|
|`script.completed`|`run_id`, `ok`|Success MUST contain `value` and no `error`. Failure MUST contain `error` and no `value`|
|`advisor.note`|`advisor`, `severity`, `note`, `review_seq`, `delivery`|Severity is `nit`, `concern`, or `blocker`. Delivery is `steer`, `aside`, or `continue`. `note` MUST be inline and at most 64 KiB|
|`thread.named`|`name`|At most 1 KiB. The latest one names the thread|
|`thread.archived`|`archived`|The latest one sets the archive flag|

Text and raw JSON fields named in this section MAY use the blob-reference form. Snapshot metadata in `tool.completed.meta` MUST remain inline and MUST NOT exceed 64 KiB.

### Image attachments

A `user.message` MUST carry at most 10 images in its `images` member.
Each image MUST name a blob of PNG, JPEG, or GIF data.
An image MUST NOT use an inline payload.
An image path MUST be masked before the event is journaled.
A `verbatim` message carries its images like any other message.

## Fold state

The fold MUST use one of these states: `idle`, `turn_running`, `awaiting_approval`, or `damaged`. An illegal transition MUST return an error and a damaged state. It MUST NOT mutate the input state.

After the state becomes `damaged`, every later fold MUST return an error.

The state MUST track:

- the thread and open turn.
- assistant calls without `tool.started`.
- started calls without `tool.completed`, including their class and arguments.
- unresolved approvals and declined calls.
- open script runs, their recorded effect indexes with `op` and `args_sha`, and the next effect index.
- spawned subagents.
- the latest `runner.bound` and `compaction`.
- the current todo list, thread grants, read snapshots, and completed-turn sequence numbers.
- delivered advisor notes, keyed by advisor, `review_seq`, and note text.

The fold MUST set `Preview` to the first 80 Unicode characters of the first user message of kind `prompt` with inline text. Until `Preview` is set, the fold sets `PreviewBlob` to the text blob of the first user message of kind `prompt` that has one.

`turn.started` MUST NOT overlap another turn. Turn-scoped events MUST name the open turn. `turn.completed` MUST NOT leave calls, approvals, or scripts unfinished. `tool.completed` MUST have a matching `tool.started`, except for an unsuccessful skipped call or an unsuccessful call declined by an approval. Those exceptions MUST clear the pending assistant call.

An approval resolution MUST have a matching unresolved request. The fold MUST create the thread grant for a user `accept_session` decision when the request carries `class`. A request without `class` MUST defer the thread grant to the `tool.started` of the call. For `exec` the thread grant is an argv prefix derived from one command. For an edit or remote tool the thread grant is that tool name. Agent, read, and script calls MUST NOT create thread grants. A later `grant:<prefix>` decision MUST NOT create a thread grant.

`child.spawned` MUST name an in-flight agent call or a call of an open script. A branch MUST occur while idle. A nonzero `parent_seq` MUST precede the branch and identify a completed turn. A script effect index MUST NOT repeat. A repeated index with a different `op` or `args_sha` MUST report the mismatch.

A script call has `call_id = "<run_id>/<n>"` for an open run. Its `approval.requested`, `tool.started`, `tool.completed`, and `child.spawned` MUST be accepted without an assistant call. A call that Cursor ran in band MUST appear in `assistant.message` with `resolved = true`. Its approval, `tool.started`, and `tool.completed` follow the message in one batch.

`advisor.note` MAY occur inside or outside a turn. A second note with the same advisor, `review_seq`, and note text MUST be rejected.

Read metadata MUST restore direct snapshot objects and snapshot objects inside `captures`. A successful `todo` completion MUST replace the todo list from its started arguments.

## Active paths and turn trees

`ActivePath` MUST walk backward from the journal head. It MUST include ordinary events and exclude `branch` control events. At a branch, it MUST jump to `parent_seq` and continue backward. A zero parent MUST select the root. A missing or forward parent MUST stop the walk. The result MUST use journal order.

`Tree` MUST contain one node per `turn.started`. A new turn MUST name the then-current leaf as `ParentTurnID`. A branch MUST move the current leaf to its completed turn or to the root. `Preview` MUST contain at most the first 80 Unicode characters of the turn's first prompt. `Active` MUST match the turns on the final active path.

Model context and thread reads MUST use only the active path. Runner bindings, thread grants, snapshots, and recovery metadata MUST fold over the full journal.

## Model context

The agent MUST build each model request from the active path after the latest `compaction`: the compaction summary first, then the events with `seq >= first_kept_seq`.

- A `user.message` or `advisor.note` journaled after the `context_seq` of the next `assistant.message` MUST render after that assistant message. The request for that assistant message does not carry the input.
- A user message or a `steer` or `continue` note that arrives while tool results are outstanding MUST render after those results.
- A `followup` message MUST render after the first assistant message without tool calls that follows it, or at the turn end.
- An `aside` note MUST render inside the next rendered user message.
- An advisor note MUST render as `<advisory advisor="<name>" severity="<severity>">note</advisory>`, with `&`, `<`, `>`, `"`, and `'` HTML-escaped in every value.
- Tool results MUST render in call order. A pruned call MUST render the output `[output pruned to fit the context window]`.
- An image of a `user.message` MUST render in the same model message as the text of that message.

The model MUST be called again when the rendered context ends with user content.
