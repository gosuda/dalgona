# Native client protocol

This document specifies required extensions for the Gio client. These extensions do not exist merely because this specification names them.
The source baseline is `2c036c60b379e7718255a23ebd2c5079d76ec50c`.

The existing [core protocol](../../dal/spec/protocol.md) remains authoritative for unchanged methods.
Native extensions use that JSON-RPC connection and its bidirectional requests.
There is no separate HTTP API or desktop SDK.

Read [architecture](native-architecture.md), [components](native-components.md), [documents](native-documents.md), and [platform behavior](native-platform.md) alongside this contract.
All required capability families are release gates.

## R01. Existing method mapping

The methods below exist at the baseline unless explicitly marked as new.
The native client MUST preserve their semantics rather than infer behavior from a similar method name.

| User operation | Existing method | Material constraint |
| --- | --- | --- |
| Initialize | `initialize` | Current reply has no native capability contract. |
| New thread | `thread/start` | Model, working directory, approval, and sandbox retain core validation. |
| Resume thread | `thread/resume` | Native callers send `excludeTurns:true` before opening paged history. |
| Read thread | `thread/read` | Native callers send `includeTurns:false` and obtain content through R05. |
| List threads | `thread/list` | Decimal offset cursors can overlap after concurrent updates. |
| Page turns | `thread/turns/list` | `summary` equals `full`. Transfer pagination does not prove bounded journal reads. |
| Rename | `thread/name/set` | Uses the thread identifier. |
| Archive or restore | `thread/archive`, `thread/unarchive` | Archive can invoke destructive runner cleanup. |
| Compact | `thread/compact/start` | Runs the existing compaction operation. |
| Revert or fork | `thread/revert`, `thread/fork` | Requires an idle thread. |
| Inspect or select branch | `dalgona/thread/tree`, `dalgona/thread/leaf/set` | Leaf changes require an idle thread. |
| Send prompt | `turn/start` | Requires an idle thread. |
| Steer running work | `turn/steer` | Send `expectedTurnId`. The server checks it atomically. |
| Interrupt | `turn/interrupt` | Targets the exact thread and turn. |
| Review | `review/start` | Supports existing target variants and inline delivery only. |
| Models | `model/list` | Qualified identifiers and `inputModalities` control selection and attachments. |
| Settings | `config/read`, `dalgona/settings/read` | These expose defined settings, not arbitrary configuration files. |
| Write settings | `config/value/write`, `config/batchWrite` | Batch writes are atomic and support `expectedVersion`. |
| Existing account flows | `account/read`, `account/login/start`, `account/login/cancel`, `account/logout` | Existing OAuth support covers ChatGPT. Other flows need the new family. |
| Existing provider inventory | `dalgona/account/providers/list` | Some providers currently advertise terminal login. |
| Skills | `skills/list` | Reads backend skills. |
| MCP state | `mcpServerStatus/list` | `threadId` selects workspace state. Unstarted servers are absent. |
| Execute command | `command/exec` | No thread or PTY. Output is merged and unmasked. |
| Stop command | `command/exec/terminate` | Signals an existing connection-owned process. |
| Keywords | `dalgona/keywords/list`, `dalgona/keywords/match` | Match input is limited to 64 KiB. |
| Search threads | `dalgona/thread/search` | Uses the repairable journal mirror. |
| Advisors | `dalgona/advisor/status`, `dalgona/advisor/set`, `dalgona/advisor/reset` | Retains backend review and enablement rules. |
| Legacy spilled output | `dalgona/item/output/read` | Reads the journal and resolves by item ID without a turn-scoped snapshot. Native readers use R05. |
| Host files | `fs/readFile`, `fs/writeFile`, `fs/createDirectory` | Operates on daemon-host absolute paths. |
| Host metadata | `fs/getMetadata`, `fs/readDirectory` | Does not route through a thread runner. |
| Host changes | `fs/remove`, `fs/copy`, `fs/watch`, `fs/unwatch` | Watches and mutation defaults retain core behavior. |
| Host file search | `fuzzyFileSearch` | Does not become runner-aware through a GUI label. |
| Daemon identity | `dalgona/daemon/identity` | Exists before initialization. Current identity lacks a native instance identifier. |
| Explicit retirement | `dalgona/daemon/retire` | Stops the shared daemon. |
| Thread box pull | `dalgona/remote/pull` | Product extension returns `{exitCode, output}`. |
| Runner cleanup | `dalgona/remote/gc` | Product extension returns `{exitCode, output}`. |

Core registration lives in [serve.go](../../../internal/dal/rpc/serve.go).
Product remote methods are registered in [cmd/dalgona/remote.go](../../../cmd/dalgona/remote.go).
They are existing product extensions even though the core method table omits them.

The native file panel MUST use the new workspace methods.
The terminal MUST use the new terminal methods.
Neither panel may fall back to host `fs/*` or `command/exec` for a remote thread.

## R02. Common types and limits

All members use camelCase. Required members cannot be null.
An omitted optional member differs from an empty value where the field contract says so.
Unknown response members remain ignorable under the core compatibility policy.

Every text field MUST follow [Input text](../../dal/spec/input-text.md), including exact preservation and validation before effects.
Byte offsets MUST identify their original source and revision. They MUST NOT be interpreted as runes, UTF-16 units, or grapheme counts.
Field-specific path and credential grammars remain authoritative.
Shared text-policy failures use the core text error contract before mutation reservation or external work.
Method-specific grammars retain their declared parameter errors.

| Type | Wire contract |
| --- | --- |
| `Id` | Opaque nonempty UTF-8 string, at most 128 bytes. |
| `Handle` | At least 128 random bits encoded as unpadded base64url, at most 128 bytes. |
| `Revision` | Opaque nonempty string, at most 128 bytes. Equality is its only client operation. |
| `Offset` | Decimal string representing an unsigned 64-bit byte offset. |
| `Ordinal` | Decimal string representing an unsigned 64-bit event position. |
| `OperationId` | UUIDv7 with its creation timestamp. |
| `Timestamp` | UTC RFC3339 with millisecond precision. |
| `Digest` | Lowercase SHA-256 encoded as 64 hexadecimal characters. |
| `RelativePath` | UTF-8 path relative to a granted root, at most 4096 bytes. |
| `ErrorData` | `{kind, operationId?, stage?, currentRevision?, retryAfterMs?}`. |
| `Mutation` | An operation requiring a durable reservation before its external effect. |

Offsets and ordinals use strings to avoid JavaScript integer precision loss in the shared protocol.
Relative paths use `/` separators on every platform.
They reject absolute forms, drive prefixes, NUL, empty interior components, `.` components, and `..` components.
The empty path denotes the granted root only for read operations.

| Budget | Required value |
| --- | --- |
| Existing JSONL frame | At most 16 MiB, including the terminating newline. |
| Ordinary native result | At most 2 MiB encoded JSON, unless explicitly specified. |
| Native event envelope | At most 256 KiB encoded JSON. |
| Native item header | At most 64 KiB encoded JSON. |
| Inline text delta | At most 64 KiB UTF-8, also within the event envelope limit. |
| Live content metadata | At most 4096 references and 4 MiB per connection. |
| Pending client mutations | 64 per endpoint. |
| Native thread subscriptions | 8 per connection. |
| Snapshot handles | 8 per connection and 64 per daemon. |
| Workspace handles | 8 per connection. |
| Workspace watches | 32 per connection. |
| PTY sessions | 4 per connection and 16 per daemon. |
| Authentication transactions | 4 per connection, with one transaction per credential target. |
| Diagnostic output | 1000 rows and 1 MiB encoded JSON. |

Byte limits apply after decoding unless a row explicitly says encoded JSON.
Base64 transfer budgets MUST account for expansion and envelope overhead.
The existing 16 MiB raw filesystem allowance is not a safe 16 MiB JSONL payload.

Every collection MUST have a count and byte bound.
Oversized required state returns `limit_exceeded` without a partially successful mutation.
The daemon MUST NOT silently discard security decisions, transcript content, or final process status.

## R03. Native capability negotiation

The GUI sends this addition inside `initialize.params.capabilities`:

```json
{"dalgonaNative":{"version":1,"delivery":"ordered-v1"}}
```

Successful negotiation adds `dalgonaNative` to the initialization result.
It contains `{version:1, delivery:"ordered-v1", daemonInstanceId, streamId}`.
Both identifiers are fresh after daemon restart. The stream identifier is fresh for every connection.

The new `dalgona/capabilities/read {}` method returns:

```json
{
  "version": 1,
  "daemonInstanceId": "opaque-instance",
  "backendBuild": "0123456789abcdef",
  "platform": {"os":"linux","arch":"amd64","pathStyle":"posix"},
  "features": {
    "orderedEvents":1,
    "requestCancellation":1,
    "history":1,
    "mutationReceipts":1,
    "imageRead":1,
    "threadUnsubscribe":1,
    "auth":1,
    "mcpConfig":1,
    "workspace":1,
    "terminal":1,
    "runnerManagement":1,
    "diagnostics":1,
    "failureRecords":1,
    "reporting":1
  },
  "receiptMethods": [],
  "terminalCheckpointVersion":1,
  "limits": {
    "frameBytes":16777216,
    "resultBytes":2097152,
    "subscriptions":8,
    "snapshotHandles":8,
    "workspaceHandles":8,
    "terminalSessions":4
  }
}
```

`receiptMethods` MUST list every mutation covered by R07. The empty array above shows shape only.
`mcpConfig:1` includes the inventory-version and runtime-source extension in R10.
`workspace:1` includes explicit workspace release. `terminal:1` includes resize replay, final-output retention, and attachment detach.
All listed feature versions are required for the complete native release.
A missing feature or unsupported checkpoint version makes that endpoint incompatible.
The GUI MUST offer a backend update or endpoint switch instead of silently reducing required functionality.

Negotiation applies to one connection only.
Clients that omit the native capability retain existing notifications and response schemas.
The implementation MUST add native decoding to the shared wire owner.
It MUST NOT require a second parallel connection for new features.

## R04. One ordered delivery stream

An opted-in connection receives thread notifications only through `dalgona/event`.
It MUST NOT also receive their legacy unwrapped copies.
Non-thread lifecycle responses remain ordinary JSON-RPC responses.

The event parameters are:

```json
{
  "streamId":"opaque-stream",
  "firstOrdinal":"41",
  "lastOrdinal":"41",
  "threadId":"thread-id",
  "method":"item/agentMessage/delta",
  "params":{"threadId":"thread-id","turnId":"turn-id","itemId":"item-id","delta":"text"}
}
```

The daemon assigns increasing ordinals through one connection dispatcher.
Ordinals cover all subscribed thread notifications and thread-scoped server requests.
No ordinal is reused within a stream.
Client responses do not allocate server ordinals.

Native server requests use the negotiated JSON-RPC method `dalgona/request` with the original JSON-RPC identifier.
Parameters are `{method, details, dalgonaEvent:{streamId, ordinal, threadId}}`.
`method` names the original approval, question, or workspace-permission request.
`details` is `{requestId, total, digest}` for the immutable original parameter JSON.
This header is at most 4096 bytes.
The session worker processes request headers in the same ordinal order as notifications.
It fetches the original parameters through R06 before exposing actionable decisions.
Its response still answers the original identifier with the original response schema.
Legacy clients retain the unwrapped original server request.

Native item-bearing events use the same `ItemHeader` type as history pages.
This includes `item/started`, `item/completed`, catch-up, and replay delivery.
An item header contains `{id, type, itemRevision, metadata, contents}`.
Bulk text, reasoning, arguments, patches, and output use the content references defined in R05.
Completion atomically replaces the provisional item version with its authoritative header and content references.
It MUST NOT append completed text to the already received deltas.

Native projection MUST preserve journal blob and spill descriptors before legacy text or argument hydration occurs.
Wrapping a fully materialized legacy `Item` after loading its blob is insufficient.
The native path MUST NOT construct or serialize the complete referenced body to emit its header.
Legacy clients retain their existing item representation.

Every native event obeys the encoded envelope limit, including non-item events.
An otherwise oversized parameter object uses `paramsContent` instead of `params`.
`paramsContent` is one R05 content reference with `field:"params"` and `encoding:"json"`.
The two members are mutually exclusive.
The referenced parameter object is immutable at that event ordinal.
It uses bounded metadata and nested content references wherever its normal projection contains bulk item fields.
The reader materializes bounded headers and pages document fields instead of reconstructing an unbounded event in the mailbox.

The projection splits oversized incoming text deltas before assigning native ordinals.
Splits preserve UTF-8 character boundaries and exact text order.
They MAY split an extended grapheme cluster between deltas.
The client MUST retain source text and resegment the affected tail before layout or editing.
Delta boundaries MUST NOT create artificial caret stops, replacement characters, or duplicate content.
Each resulting event receives its own ordinal.
Only consecutive text deltas for the same item and field may share one envelope.
Its ordinal range MUST cover exactly the merged consecutive messages.
Unlike legacy coalescing, native coalescing MUST NOT cross an intervening decision or unrelated event.
Coalescing stops before either the delta byte limit or encoded envelope limit would be exceeded.
Opening or replacing a history snapshot inserts a coalescing barrier in the same dispatcher.
The dispatcher finishes its pending envelope before selecting that snapshot's cut.
No queued or emitted envelope may satisfy `firstOrdinal <= cutOrdinal < lastOrdinal` for that cut.
This is an internal ordering boundary, not another wire event or journal record.
The client applies the concatenated text once and advances to `lastOrdinal`.

A gap, overlap, wrong stream identifier, or impossible range invalidates the live projection.
The GUI MUST reconnect and obtain a new snapshot.
It MUST NOT guess missing text or continue from `thread/status/changed.seq`.

The native stream uses a fresh connection ordinal, not a durable journal cursor.
`thread/status/changed.seq` retains its existing status-only meaning.
Reconnect always establishes a new stream and authoritative snapshot.
It does not request replay by an old ordinal.

The server writer queue retains the core 4096-message bound and adds a 16 MiB encoded byte bound.
A full queue closes the connection.
The session worker MUST treat this closure as recoverable state loss with uncertain dispatched mutations.

## R05. Bounded history and atomic subscription

Native `thread/resume` MUST send `excludeTurns:true`.
Native `thread/read` MUST send `includeTurns:false`.
After `history:1` negotiation, start and fork responses and `thread/started` notifications MUST contain `turns:[]`.
They retain effective settings, usage, sandbox fallback, and all required thread metadata.
This bounded native bootstrap MUST avoid replaying or serializing the complete conversation before opening history.
Paged history is the native path for existing conversation content.
Legacy clients retain their existing response and notification behavior.

The following methods are new:

| Method | Parameters | Result |
| --- | --- | --- |
| `dalgona/history/open` | `{threadId, replaceHistoryId?}` | `HistorySnapshot`. |
| `dalgona/history/page` | `{historyId, cursor?, direction?, limit?}` | `{turns, nextCursor?, previousCursor?, pathRevision}`. |
| `dalgona/history/content/read` | `{source, contentId, offset?, limit?}` | `{dataBase64, offset, next, total, digest}`. |
| `dalgona/history/close` | `{historyId}` | `{closed:true}`. |

`HistorySnapshot` contains `{historyId, thread, pathRevision, streamId, cutOrdinal, activeTurnId?, pendingRequests, newestCursor, oldestCursor, expiresAt}`.
`thread` uses the existing thread metadata without the unbounded `turns` member.
`pendingRequests` contains at most 64 unresolved decision headers, bounded to 256 KiB total.
Each header contains `{requestId, method, details:{total, digest}}`.
Full commands, diffs, permission scopes, and question text remain in authorized immutable detail references.
The snapshot MUST NOT truncate an approval diff or omit a valid decision to meet its header budget.

Opening a snapshot MUST atomically register the subscription and capture its projection cut.
The snapshot contains all durable state and accumulated active-item content at that cut.
Subsequent delivery contains all changes after the cut.
The server MUST NOT reconstruct this cut from status sequence numbers.

The connection dispatcher MUST apply all earlier source events to the projection and finish pending coalescing before freezing the snapshot.
Its `cutOrdinal` is the last ordinal assigned before the barrier, or `"0"` when none exists.
The immutable snapshot MUST be captured before later events can update that projection.
This rule applies to both initial history opens and replacement snapshots.

The session worker buffers events while the open response is pending.
It MUST preserve received envelope boundaries until the pending snapshot cut is known.
Client-side coalescing MUST NOT merge envelopes across a pending or established snapshot cut.
The worker validates connection-wide ordinal continuity before applying each thread's cut filter.
After applying the snapshot, it discards this thread's envelopes whose `lastOrdinal <= cutOrdinal`.
It applies envelopes whose `firstOrdinal > cutOrdinal` in stream order.
A straddling envelope violates R04 and requires a new connection and snapshot without applying any guessed text suffix.
Other thread projections retain their own cuts while sharing the connection ordinal sequence.
For pending requests, discarding duplicate presentation MUST preserve the original transport responder.
The request identifier attaches that responder to the decision already represented by the snapshot.

Opening history on an already subscribed thread replaces that thread's projection cut without adding another subscription.
Existing start and resume methods continue subscribing under their core semantics.
The native client still opens history before presenting a reconciled transcript.

Each snapshot freezes the active path revision and visible item versions.
Later appends, edits, forks, and compactions do not change its pages.
Live events update the current projection separately.
The client MUST show unseen appended items without inserting them into an older frozen page.
Changing branches opens a new snapshot and releases the previous handle.

`replaceHistoryId` requests an atomic replacement within the old snapshot's existing slot.
The server validates that the old handle belongs to this connection and thread.
It captures and validates the replacement before publishing the new handle and retiring the old handle.
Failure leaves the old snapshot and subscription intact.
Successful replacement changes no subscription count and requires no ninth snapshot slot when eight slots are occupied.
Both preparation and publication remain within the snapshot storage budgets.

The application holds a local snapshot lease for the complete duration of copy, Find, or export.
It MUST NOT request replacement of a handle held by such a consumer.
Active server reads also prevent retiring their handle until the reads finish or cancel.
When the old snapshot is leased, a fresh snapshot requires a separate free slot.
If no slot exists, the server returns `limit_exceeded` without invalidating the old snapshot or evicting another thread.
The application shows that capacity failure and preserves the current view.
The application waits for the existing consumer or obtains an explicit cancellation decision before retrying.

`direction` is `older` or `newer`, with `older` as default.
`limit` counts turns, defaults to 20, and cannot exceed 100.
A page cannot exceed 1 MiB encoded JSON.
It MAY contain fewer turns to meet the byte budget.
A page continuation MUST preserve progress when one turn contains many items.

Each returned turn page includes `{turnId, turnRevision, firstItemIndex, nextItemIndex, itemsComplete, items}` alongside its turn metadata.
Indices are zero-based and `nextItemIndex` is exclusive.
`itemsComplete:true` means the final item of that frozen turn is included.
The first page of a turn has `firstItemIndex:0`.
Later pages MUST begin at the preceding `nextItemIndex` and retain the same turn revision.
The client appends by item position and identity instead of replacing an earlier partial turn.
It rejects gaps, conflicting repeated items, and revision changes within one snapshot.

Each turn includes its existing metadata and bounded item headers.
`ItemHeader` contains `{id, type, itemRevision, metadata, contents}`, identically in history pages and native live events.
`metadata` retains scalar identifiers, status, timestamps, and small typed fields from the core item schema.
Bulk text, reasoning, command arguments, patches, and outputs move into content references.
Each reference is `{source, contentId, field, encoding, total, digest}`.
`field` is the original item field name. `encoding` is `utf8`, `json`, or `binary`.
Images retain their indexed image metadata and use R08 for bytes.

`source` is the discriminated union `ContentSource`:

```json
{"kind":"snapshot","historyId":"snapshot-handle"}
{"kind":"live","streamId":"connection-stream"}
```

Snapshot references bind the frozen snapshot and item revision.
Live references bind the connection stream, subscribed thread, originating ordinal, source revision, byte length, and digest.
An item reference additionally binds its turn, item, and field.
Both source kinds identify immutable bytes.
A live reference MUST NOT follow later appends or silently resolve to the item's newer revision.
Only the single content-read method serves both kinds.

Output content references MUST include the complete masked logical output, including stored spill bytes beyond an inline preview.
Each reference binds endpoint, thread, turn, item, source revision, byte length, and immutable content digest.
The backend MUST resolve that exact spill through its journal index and read only the requested range.
Equal item identifiers in different turns MUST remain distinct.
Neither branch changes nor later output appends may redirect an existing reference to another spill or version.
The native reader MUST NOT use legacy `dalgona/item/output/read` or expose a raw spill path.
Legacy clients retain that method's existing behavior.

An individual metadata string cannot exceed 4096 bytes.
An item header cannot exceed 64 KiB encoded JSON.
Larger variable content MUST become a content reference, not a truncated header.
The page cursor also encodes the continuation within a turn's item list.

Content reads default to 64 KiB and permit at most 256 KiB.
Offsets address the immutable referenced bytes, including UTF-8 bytes.
The decoder MUST tolerate UTF-8 sequences split between pages.
Zero-length content has `offset`, `next`, and `total` equal to `"0"`.

Cursors bind the snapshot, connection, thread, endpoint, and path revision.
Content identifiers additionally obey their selected source's ownership and lifetime.
They MUST NOT reveal an arbitrary file or blob address.
Foreign, malformed, or expired cursors fail before storage access.
Handles expire after five minutes without a successful history request.
Closing or disconnecting releases their resources immediately.
An actively displayed view refreshes its snapshot lease through a bounded page read before that deadline.

Live references expire after five idle minutes, unsubscribe, or connection closure.
A successful live content read renews that reference's idle lease.
Their metadata obeys the reference count and byte limits in R02.
Capacity pressure may expire the least recently used live reference when no read currently pins it.
It MUST NOT discard the underlying journal content or invalidate a snapshot reference to that content.
An expired live reference returns `expired` without closing the healthy connection.
The client obtains a fresh snapshot containing the item and retries through its frozen source.
It MUST NOT reconnect repeatedly to refetch an oversized live event.

The server MUST use the existing immutable index or bounded reference metadata.
It MUST NOT add an unbounded registry of live items or retain one copied body per notification.
Live spill data shares the storage budgets below and remains pinned while an admitted read needs it.
An export that includes live items first obtains a frozen snapshot covering its selected scope.

The implementation MUST use a journal index and immutable content references for older turns.
Repeated pagination MUST NOT reread or refold the complete journal.
An active item uses a bounded accumulator with spill storage before a snapshot is exposed.
Its in-memory portion is at most 256 KiB per active item.
Snapshot-specific spill storage is capped at 256 MiB per thread and 1 GiB per daemon.
Capacity refusal leaves existing snapshots valid and returns `limit_exceeded`.

These indexing and active-content contracts are new backend work.
The existing subscription replays pending questions but does not provide a complete accumulated partial-text snapshot.
The implementation MUST close that gap before advertising `history:1`.

Cancelling a history request releases its temporary scan and allocation work.
It does not interrupt the thread or invalidate a previously issued snapshot.

Find, selection, copy, and export MUST traverse the complete selected snapshot scope through these source references.
They MUST NOT silently restrict results to mounted UI pages.
The client streams content with bounded buffers and preserves matches across content-chunk boundaries.
An export writes a temporary destination and publishes it only after the complete requested scope is read.
Snapshot expiry during a read fails visibly and offers a fresh snapshot with explicit scope confirmation.
It MUST NOT combine bytes from different path revisions into one exported document.

## R06. Detachment and decisions

Implement the existing upstream method `thread/unsubscribe {threadId}`.
Its result is `{status:"unsubscribed"|"notSubscribed"|"notLoaded"}`.
The enums come from the vendored Codex [unsubscribe response](../../../internal/dal/rpc/testdata/codex-schema/v2/ThreadUnsubscribeResponse.json).
The baseline includes that schema but no registered handler.

Unsubscribe removes only this connection's thread subscription and history handles.
It does not archive the thread, destroy a runner, stop a terminal, or interrupt a turn.
Repeated unsubscribe is harmless and returns the appropriate status.

Approvals retain the core server request schemas and first-answer-wins behavior.
Questions retain those schemas and the first-valid-answer rule in [Ask tool](../../dal/spec/ask.md).
The GUI MUST distinguish approval decisions from freeform user questions.
It MUST NOT answer a stale request after `serverRequest/resolved`.
The shared connection owner MUST cancel the matching request context when the resolved notification appears inside a native envelope.

Question text validation and correction warnings MUST follow the [core protocol](../../dal/spec/protocol.md#server-requests).
The typed question lifecycle MUST keep its response sender and draft after the first send until resolution.
A matching correction warning MUST permit another response under the same request ID and original deadline.
It MUST NOT create another question, consume the pending waiter, or add a journal event.
The shared connection owner MUST migrate its current one-response callback path for this lifecycle.
Native envelope ordering MUST preserve each warning and resolution without reopening an already resolved card.

Reconciliation preserves pending request identifiers at the snapshot cut.
A request already represented by the snapshot MUST NOT create a second modal when its earlier delivery finishes.
Resolution removes the request by identity across every window.

If other subscribers remain, their decision requests remain active.
The last subscriber leaving can decline an approval or finish a question with no client.
The GUI MUST NOT promise that an unanswered question survives its sole connection closing.
The existing five-minute question timeout remains unchanged.

The new `dalgona/request/details/read {requestId, offset?, limit?}` reads the original server request parameters.
Its result is `{offset, next, total, digest, dataBase64}`.
The default limit is 64 KiB. The maximum is 256 KiB.
The request identifier is scoped to this authenticated connection and pending request.
Foreign identifiers fail before detail lookup.
Resolution or cancellation expires the reference and cancels unfinished reads.

Original parameters may occupy up to 16 MiB.
The backend stores immutable references or bounded spill data instead of embedding them in snapshots or dispatch mailboxes.
The GUI verifies total size, digest, original schema, and pending identity before enabling a response.
It renders large diff details through the virtualized document model.
It MUST NOT derive decision authority from a truncated preview.
Workspace permission requests use this same mechanism and appear in pending snapshots.

## R07. Mutation outcomes and exact errors

New externally effectful methods use `operationId` unless a section defines a stronger session-local sequence.
Idempotent connection-owned cleanup is excluded.
That exclusion covers history close, unsubscribe, auth cancel, workspace unwatch, and upload abort.
It also covers workspace close and terminal attachment detach under R11 and R12.
Those calls release existing resources and never create a replacement effect.
Negotiated existing mutations accept `dalgonaOperationId`.
This member is legal only after `mutationReceipts:1` and exact method membership in `receiptMethods`.

Covered existing methods are `thread/start`, `thread/name/set`, `thread/archive`, `thread/unarchive`, `thread/compact/start`, `thread/revert`, and `thread/fork`.
They also include `turn/start`, `turn/steer`, `turn/interrupt`, `review/start`, `dalgona/thread/leaf/set`, and the advisor mutations.
Both config write methods and both remote management mutations are covered.
Native clients MUST NOT assume older backends honor an ignored operation identifier.

The server reserves an operation identifier and canonical parameter digest durably before dispatching its external effect.
The key is scoped to the authenticated product home and endpoint identity.
Its method and parameter digest cannot change.
Reuse with another digest returns `operation_conflict`.

The new `dalgona/operation/read {operationId}` returns `{state, method, result?, error?, expiresAt}`.
`state` is `reserved`, `running`, `succeeded`, `failed`, or `unknown`.
Final `result` and `error` reproduce the original bounded outcome.
Authentication receipts exclude submitted secrets and token-bearing responses.

Completed operations return the recorded result on exact repetition.
An in-progress duplicate returns `busy` with its operation identifier.
A crash after reservation can leave `unknown`.
The daemon MUST NOT dispatch an unknown operation again automatically.

Receipts remain for 25 hours.
New identifiers more than 24 hours old or five minutes in the future are rejected as invalid parameters.
The store permits 10000 receipts and 32 MiB.
Capacity exhaustion refuses new mutations before effects occur.
It never evicts an unexpired reservation to admit new work.

Thread-related outcomes use journal-correlated durable records.
Other outcomes use an atomically published private receipt store.
Receipt failures MUST fail the operation before dispatch or preserve unknown outcome after dispatch.
No exactly-once claim extends beyond a known committed outcome.

The client marks a mutation `unknown` when transport ends after dispatch without a conclusive response.
Its Go error is typed `UnknownOutcomeError` with method, endpoint, operation identifier, and dispatch generation.
Cancellation after dispatch has the same uncertainty unless the server confirms otherwise.
Read-only calls may be retried after reconnect within their deadlines.
Unknown mutations require receipt inspection and reconciliation before any explicit retry.

| Code | Exact `kind` | Required meaning |
| --- | --- | --- |
| `-32602` | `invalid_params` | Reject malformed input before effects. |
| `-32010` | `unavailable` | The target or required service cannot currently be reached. |
| `-32011` | `incompatible` | Protocol, backend, or checkpoint version cannot satisfy the request. |
| `-32012` | `stale_revision` | An expected file, config, or binding revision changed. |
| `-32013` | `permission_denied` | Required authority was absent, declined, or revoked. |
| `-32014` | `limit_exceeded` | A documented byte, count, or resource budget would be exceeded. |
| `-32015` | `expired` | A handle, lease, or transaction is no longer valid. |
| `-32016` | `busy` | An operation or exclusive resource is already active. |
| `-32017` | `unknown_outcome` | Dispatch occurred without a provable final outcome. |
| `-32018` | `replay_gap` | Required stream bytes are outside retained output. |
| `-32019` | `cancelled` | Cancellation completed before any unreported effect. |
| `-32020` | `not_found` | The authorized lookup found no such object. |
| `-32021` | `operation_conflict` | One operation identifier was reused with different input. |

Shared encoding, size, line-break, control, and offset failures are an explicit exception to the table's generic parameter and resource errors.
They MUST use `-32602` with `invalid text: <field>: <reason>` under the core protocol.
If structured error data is present, its `kind` is `invalid_params`.
This precedence includes a decoded text field exceeding its byte or scalar quota.
Collection counts, aggregate transfer budgets, and nontext resource limits retain `limit_exceeded`.
Method-specific grammar and schema failures retain their declared errors. A path traversal or unsupported enum is not an encoding failure.
Malformed JSON, UTF-8, or surrogate escapes retain the core `-32700` parse error before dispatch.

For other new errors, `message` equals the exact `kind` string.
The optional structured fields provide bounded detail without secret-bearing raw causes.
Existing methods retain existing errors for legacy clients.
An error's name alone never permits mutation retry.

## R08. Referenced image retrieval

The new method is `dalgona/item/image/read {threadId, turnId, itemId, imageIndex, offset?, limit?}`.
Its result is `{mimeType, digest, total, offset, next, dataBase64}`.
The image index is zero-based within the item's image list.
The default limit is 256 KiB. The maximum is 1 MiB.

The server resolves an image only through the authorized thread, turn, and item tuple.
An item identifier may repeat in different turns.
The image cache key includes endpoint, thread, turn, item, image index, and digest.
The method MUST NOT accept a raw blob hash, path, or arbitrary URL.
A valid `dalgona-blob:` reference does not itself grant access.
The client verifies the final digest and MIME signature before rendering.

Input attachment limits remain ten images, 5 MiB each, and 10 MiB combined.
Only the existing PNG, JPEG, and GIF input formats are accepted.
The GUI MUST enforce those limits before base64 encoding.
Remote attachment upload uses inline bytes when the selected file exists only on the GUI host.

The decoder rejects dimensions beyond 16384 pixels per side or 32000000 pixels total.
Decoding runs in the bounded worker and cannot exceed 128 MiB of decoded allocation.
Animated images additionally obey the document decoder budget.
The GUI keeps outgoing JSON messages within 15 MiB, below the transport framing ceiling.
Decoder failure produces a visible attachment error without affecting the transcript.
Image bytes and thumbnails MUST NOT enter diagnostic logs.

## R09. Provider, MCP, and reporter authentication

New authentication types are:

| Type | Fields |
| --- | --- |
| `AuthTarget` | Provider: `{kind:"provider", id}`. MCP: `{kind:"mcp", name, threadId?}`. Reporter: `{kind:"reporter", id:"failure-reporting"}`. |
| `AuthFlow` | `apiKey`, `browserCallback`, `manualCode`, `deviceCode`, or `cursorDeepLink`. |
| `AuthDescriptor` | `{target, displayName, signedIn, accountLabel?, flows, unavailableReason?}`. |
| `AuthChallenge` | `{kind, authorizationUrl?, verificationUrl?, userCode?, prompt?, expiresAt}`. |
| `AuthStatus` | `{transactionId, target, state, challenge?, error?, expiresAt}`. |

`state` is `awaitingInput`, `waiting`, `exchanging`, `succeeded`, `failed`, `cancelled`, or `expired`.
The descriptor lists only flows implemented for that target.
Its identifiers come from backend provider definitions, never UI hardcoding.

| Method | Parameters | Result |
| --- | --- | --- |
| `dalgona/auth/providers` | `{threadId?}` | `{targets:[AuthDescriptor]}`. |
| `dalgona/auth/start` | `{target, flow, callbackMode?, operationId}` | `AuthStatus`. |
| `dalgona/auth/submit` | `{transactionId, response, operationId}` | `AuthStatus`. |
| `dalgona/auth/status` | `{transactionId}` | `AuthStatus`. |
| `dalgona/auth/cancel` | `{transactionId}` | `{status:"cancelled"|"completed"|"notFound"}`. |
| `dalgona/auth/logout` | `{target, operationId}` | `{signedIn:false}`. |

`flow` selects the request variant. The following callback rules are exhaustive.

| Flow | `callbackMode` | Input and completion |
| --- | --- | --- |
| `apiKey` | Omitted. | Start returns `awaitingInput`. Submit the exact key with `response.kind:"apiKey"`. |
| `browserCallback` | Required as `backendLoopback`. | The backend listener receives and validates the redirect. |
| `manualCode` | Required as `manualPaste`. | Submit the advertised redirect URL or authorization code. |
| `deviceCode` | Required as `device`. | The backend polls the advertised device challenge. |
| `cursorDeepLink` | Omitted. | The existing native handoff and donor polling complete the flow. |

An omitted callback mode is mandatory for the two callback-free variants, not an implicit default for other flows.
An explicit null, unsupported mode, missing required mode, or mode on a callback-free variant returns `invalid_params` before starting authentication.
The selected flow MUST be offered by the target and fit the actual browser-to-backend topology.
An API-key start MUST NOT create a callback listener, open a browser, or begin OAuth polling.
Its subsequent submit binds the same transaction and target. It accepts no redirect or authorization-code response.
API-key input expires after two minutes. Failure preserves the protected field for an explicit fresh start.
Remote backends MUST NOT advertise their loopback listener as reachable from the GUI machine.
When a browser callback cannot reach the backend, the GUI selects an offered device or manual variant for that callback-based flow.
Offered callback-free flows remain valid on remote endpoints without an OAuth fallback.
Unsupported combinations return `invalid_params` before opening a browser.

`response` is `{kind:"apiKey"|"redirectUrl"|"authorizationCode", value}`.
API keys are single-line strings of at most 4096 bytes.
Pasted redirects and authorization codes are limited to 16384 bytes.
These limits count decoded UTF-8 bytes. Single-line validation uses the shared line-break policy.
Accepted credential values MUST NOT be trimmed, normalized, or silently truncated.
An invalid candidate MUST leave the credential unchanged and remain correctable in its protected field.
The backend validates state, issuer, resource, redirect, and provider-specific exchange rules.
Secrets MUST NOT appear in logs, operation receipts, restore state, or completed status responses.

The backend owns PKCE verifiers, callback listeners, device polling, token exchange, and credential persistence.
The GUI only opens the system browser, displays challenges, and sends explicit input.
The implementation MUST share existing OAuth provider definitions and token storage.
It MUST NOT create a second refresh implementation.

All existing provider flows are required, including ChatGPT, Anthropic, Antigravity, Devin, Cursor, and Copilot.
Configured API-key providers and runner-service credentials remain supported.
API-key validation retains existing provider checks and runner-service exceptions.
Success triggers a fresh model list, without automatically resuming a failed turn.

PKCE, callback state checking, manual-code exceptions, and token exchange limits follow [oauth.md](../../dal/spec/oauth.md).
Authorization-code waiting ends after two minutes.
Device login ends after fifteen minutes and respects provider polling intervals.
Cursor keeps its donor polling and challenge behavior.
Client polling of `auth/status` never causes a second upstream poll or exchange.

Transactions belong to one connection and credential target.
Cancellation or connection closure stops their workers and callback listeners.
A token commit that already completed remains successful and is discoverable through the provider descriptor.
The client MUST NOT interpret a lost success response as proof that credentials were not saved.

MCP authentication binds registration and credentials to the canonical resource and issuer.
It retains the existing `mcp-<name>` key, lock, private mode, and atomic write rules.
Changing the resource or configured client identifier invalidates the previous usable binding.
The native login transaction becomes an additional authorized writer of the existing registration format.

Reporter App authorization uses its separate restricted identity.
It requires valid reporting consent and the configured method before authorization begins.
It cannot reuse provider, Daltoki, or ordinary GitHub credentials.

## R10. MCP configuration and runtime status

The new `dalgona/mcp/config/read {threadId?}` returns `{version, servers, trustedWorkspace}`.
Each server contains `{name, source, editable, config, effective, status?}`.
`source` is `user`, `project`, or `discovered`.
`user` identifies native product-home configuration after its config/settings merge.
`project` identifies a workspace-local entry. `discovered` identifies an imported home or plugin entry.
The loader MUST retain each entry's origin through merging and runtime registration.
It MUST preserve the first-name-wins order in [Discovery](discovery.md), including native precedence and home-before-workspace order within each source.
At most one row is effective for each exact server name. Shadowed rows MUST NOT inherit the effective row's runtime state.
The opaque `version` covers the selected workspace's complete inventory, effective entries, source classification, and precedence.
It and the runtime extension's `configVersion` use the R02 `Revision` type.
`config` is the typed native projection of the MCP configuration in [config.md](../../dal/spec/config.md).
Its stdio variant is `{kind:"stdio", command, args, env}`.
Its HTTP variant is `{kind:"http", url, bearerTokenEnv?, oauth, oauthScopes?, oauthClientId?, oauthCallbackPort?}`.
The backend maps these camelCase fields onto the existing stored configuration keys.
The transport variants are exclusive.

Values derived from secret environment variables MUST NOT be returned.
Secret-bearing literal values return `{redacted:true}` at that value's position.
Unchanged redacted values use an explicit `preserve` operation during editing.
The client cannot write the display placeholder as a secret.
An editable environment entry is `{value:string}` or `{preserve:true}`, never both.
The preserve marker is legal only for an existing redacted entry with the matching expected version.

The new `dalgona/mcp/config/write` accepts `{expectedVersion, edits, operationId}`.
Each edit is `{name, action:"put"|"remove", config?}`.
`put` supplies the complete typed user-level server entry, with explicit preservation markers where required.
The maximum is 32 edits and 256 KiB encoded parameters.
The server validates every entry before an atomic user-settings write.
Its result is `{version, restartRequiredServers}`.

Project and discovered source entries are read-only in this panel.
Editing their files requires the workspace editor and its separate authority.
Configuration changes MUST NOT start an untrusted workspace command.
Existing workspace trust requests retain command, arguments, environment-key disclosure, and approval semantics.

Runtime status uses existing `mcpServerStatus/list` with the selected thread and a negotiated source extension.
The required `mcpConfig:1` capability includes this extension as well as the configuration methods.
On that connection, requests MUST include `configVersion` from the inventory being displayed.
Responses MUST include the same `configVersion`, and every `data` row MUST include its registered `source`.
Existing name, tools, authentication, runtime status, and error fields retain their meanings.
Unnegotiated clients retain the existing request and response shape.
The vendored [status parameters](../../../internal/dal/rpc/testdata/codex-schema/v2/ListMcpServerStatusParams.json) permit additional members.
The [response schema](../../../internal/dal/rpc/testdata/codex-schema/v2/ListMcpServerStatusResponse.json) also permits them on its envelope and each status row.
The native decoder MUST validate these negotiated fields in addition to the unchanged base schema.

Status pages and cursors MUST bind the same thread and inventory version.
A changed version, or runtime registration inconsistent with that inventory, returns `stale_revision` before returning rows.
The client MUST reload inventory and discard every status page from the previous version.
A daemon advertising `mcpConfig:1` but omitting required provenance is incompatible.
The client MUST NOT guess a source from the server name, transport URL, or row order.

The panel matches a runtime row only to the effective inventory row with the same exact name and source in that version.
It MUST NOT attach runtime state to a shadowed or replacement configuration.
An absent runtime row means not started, not healthy.
Changing an active server configuration stops its old transport before the replacement starts.
In-flight calls receive a typed cancelled or unknown outcome according to their dispatch state.

The baseline [status writer](../../../internal/dal/rpc/serve_mcp.go) emits no source field.
Retaining origin in discovery and the runtime registry, then emitting negotiated provenance, is required implementation work.

## R11. Runner-aware workspace authority

Workspace operations use the thread's bound runner, including local runners.
The daemon remains the authorization owner.
The native process never obtains a direct runner object.

The new `dalgona/workspace/open` accepts `{threadId, root:"cwd", rights, operationId}`.
`rights` is a nonempty subset of `read`, `write`, and `terminal`.
The result is `{workspaceId, threadId, runnerId, bindingRevision, root, pathStyle, rights, expiresAt}`.
`root` is the backend's canonical display path, not a client-provided alternate root.

Opening requires an explicit client action naming the thread, runner, root, and requested rights.
The daemon validates the caller's trusted endpoint and requests any missing workspace permission through its decision channel.
The request displays raw-content access and command execution where requested.
The default decision is decline.
An agent tool approval does not silently authorize this handle.

The new server request is `dalgona/workspace/requestPermission`.
Its original parameters are `{threadId, runnerId, bindingRevision, root, rights, reason}`.
Native delivery adds the R04 request envelope and its ordinal metadata.
Its response is `{decision:"allowOnce"|"decline"}`.
`allowOnce` authorizes only the pending workspace handle and its stated rights.
The request expires after five minutes or connection cancellation.

The handle binds its owner connection, endpoint, thread, runner, root, rights, and binding revision.
It expires after fifteen minutes without a successful workspace or attached terminal operation.
Changing runner identity revokes it immediately.
Every operation rechecks the binding and required rights before accessing a path.

The new `dalgona/workspace/close {workspaceId}` explicitly releases an unused handle.
It returns `{status:"closed"|"notFound"}` and requires no operation identifier.
The owner connection alone may close its handle. An absent or foreign identifier returns `notFound` without changing another grant.
Repeating close is harmless and returns `notFound` after the first successful release.

Close MUST return `busy` without effects while that handle owns a terminal attachment or a dispatched workspace mutation or one-shot command.
The dispatch and close checks MUST share one ownership boundary so a new effect cannot race past successful closure.
The client MUST reconcile each dispatched operation and explicitly detach terminal attachments before releasing their workspace handle.
A pending operation with an uncertain outcome MUST retain its authority reference until reconciliation establishes that closing is safe.

Successful close cancels pending reads and searches, removes watches and cursors, aborts uncommitted uploads, and frees the workspace-handle slot before replying.
It MUST NOT delete workspace files, stop a terminal, archive a thread, or release another handle's resources.
Later operations on that handle return `expired`. Close itself retains its idempotent `notFound` result.
An already dispatched upload commit follows the busy rule and MUST NOT be mistaken for an uncommitted upload.

The shared client owner MUST count local interests from windows, file views, completion, commands, and terminal attachments.
Closing one view releases only its interest. The final interest closes the handle after its dependent operations and attachments finish.
Thread unsubscribe MUST NOT implicitly close a workspace still used by another view or terminal.
Connection loss expires its workspace handles without bypassing the separate terminal reconnect lease in R12.

| Method | Parameters | Result |
| --- | --- | --- |
| `dalgona/workspace/close` | `{workspaceId}` | `{status:"closed"|"notFound"}`. |
| `dalgona/workspace/list` | `{workspaceId, path, cursor?, limit?}` | `{entries, nextCursor?, directoryRevision}`. |
| `dalgona/workspace/search` | `{workspaceId, query, cursor?, limit?}` | `{entries, nextCursor?, incomplete}`. |
| `dalgona/workspace/stat` | `{workspaceId, path}` | `FileMetadata`. |
| `dalgona/workspace/read` | `{workspaceId, path, expectedRevision?, offset?, limit?}` | `{revision, total, offset, next, dataBase64}`. |
| `dalgona/workspace/write` | `WriteRequest` | `WriteResult`. |
| `dalgona/workspace/mkdir` | `{workspaceId, path, recursive, operationId}` | `{metadata}`. |
| `dalgona/workspace/rename` | `{workspaceId, from, to, expectedRevision, replace, operationId}` | `{metadata}`. |
| `dalgona/workspace/remove` | `{workspaceId, path, expectedRevision, recursive, operationId}` | `{removed:true}`. |
| `dalgona/workspace/watch` | `{workspaceId, path, recursive:false}` | `{watchId, directoryRevision}`. |
| `dalgona/workspace/unwatch` | `{watchId}` | `{removed:true}`. |

`FileMetadata` is `{name, kind, size, modifiedAt, revision, readable, writable}`.
`kind` is `file`, `directory`, `symlink`, or `other`.
`size` is an `Offset`. File `revision` covers content and identity, not modification time alone.
Directory revisions identify one enumerated directory state.

List limits default to 100 and cannot exceed 500 entries.
Each page is at most 1 MiB encoded JSON.
Enumeration cursors freeze one directory revision and expire after two minutes.
A changed directory produces `stale_revision` for subsequent pages instead of silently skipping entries.

Search finds paths under the same authorized runner root for file navigation and `@` completion.
`query` is at most 1024 UTF-8 bytes and contains no NUL.
Results contain `{path, score, matches}`, where matches are UTF-8 byte ranges within the returned path.
The default and maximum result count is 50.
One search visits at most 10000 eligible paths or spends five seconds, whichever arrives first.
It follows the existing file-search exclusion rules and never follows a symlink outside the root.
Results sort by descending score, then by path.
An exhausted scan budget sets `incomplete:true` without claiming a complete workspace search.
Search cursors bind the workspace revision and expire after two minutes.
The implementation MUST provide runner-side search rather than silently call host `fuzzyFileSearch`.

Read limits default to 64 KiB and cannot exceed 1 MiB.
Reads require regular files and return exact raw bytes.
The first read's revision MUST be sent as `expectedRevision` on every later chunk.
A change returns `stale_revision` before returning bytes from another version.

`WriteRequest` is a discriminated union:

```json
{"stage":"begin","workspaceId":"w","path":"a.txt","expectedRevision":"rev-or-absent","total":"12","digest":"sha256","operationId":"uuidv7"}
{"stage":"chunk","uploadId":"u","offset":"0","dataBase64":"..."}
{"stage":"commit","uploadId":"u","operationId":"uuidv7"}
{"stage":"abort","uploadId":"u"}
```

`expectedRevision` uses the literal `absent` only for create-if-missing.
Begin returns `{uploadId, offset:"0", expiresAt}`.
Chunk returns `{next}`. Commit returns `{metadata}`. Abort returns `{aborted:true}`.
Chunk size is at most 1 MiB. A transfer is at most 64 MiB.
There are at most four uploads and 128 MiB staged bytes per connection.

Chunks MUST arrive at the expected offset.
An exact repeated chunk is acknowledged without another append.
A different chunk at an accepted offset returns `operation_conflict`.
Commit verifies size, digest, binding, authority, and target revision before atomic replacement.
It preserves an existing regular file's mode and uses the runner's safe creation mode for a new file.
Failure before publication leaves the destination unchanged.
Uploads expire after five idle minutes and are removed on disconnect.

Rename defaults do not exist. The caller MUST send `replace` explicitly.
Remove defaults do not exist. The caller MUST send `recursive` explicitly.
Recursive deletion requires a fresh decision naming the root and destructive scope.
Neither operation accepts the workspace root itself.

All operations reject symlink traversal outside the granted root.
Mutation checks use directory handles or the runner's equivalent race-resistant primitive.
Checking a path and later opening it by an unchecked string is insufficient.
The runner extension MUST preserve this rule across remote exec transport.

Watch notifications use `dalgona/workspace/changed {watchId, directoryRevision, paths, rescanRequired}`.
They are invalidations, not an authoritative mutation log.
At most 1000 paths or 64 KiB may appear in one notification.
Overflow sends `rescanRequired:true` and an empty path list.
The client relists before claiming the visible directory is current.

Workspace operations require new runner and exec-server methods where current interfaces lack bounded reads, atomic commit, or safe rename.
Existing host filesystem handlers MUST NOT serve as a hidden fallback.

### Runner-scoped one-shot shell commands

The native `!` composer uses the new `dalgona/workspace/exec {workspaceId, text, operationId}` method.
`text` is nonempty UTF-8 shell text of at most 64 KiB.
The handle requires the `terminal` right, which authorizes explicit user command execution.
The backend selects the actual runner's shell, working directory, and configured sandbox policy.
It MUST NOT send a thread-runner path to host `command/exec`.

The synchronous result is `{exitCode, signal?, durationMs, outputBase64, outputTruncated, termination}`.
`exitCode` is an integer or null when no exit code exists.
`termination` is `exited`, `timedOut`, or `cancelled`.
The command has a 120-second execution deadline and retains at most 64 KiB of merged raw output.
Additional output is drained and sets `outputTruncated:true`.
The native request deadline is 125 seconds, allowing bounded termination and final receipt persistence.

There are at most four concurrent one-shot commands per connection.
Their output is unmasked, user-requested data and never enters the transcript journal automatically.
The UI labels truncated output and cannot export it as complete output.
The command is not a PTY and accepts no interactive stdin.
Interactive commands belong in the actual terminal component.

An explicit Stop sends negotiated request cancellation while retaining the original response waiter.
Cancellation or connection closure terminates the owned process group under the runner's bounded stop policy.
The daemon persists the final outcome or records uncertainty before releasing the operation reservation.
The client never restarts an uncertain command automatically.

## R12. Real terminal sessions

The terminal uses a Unix PTY or Windows ConPTY on the actual execution target.
Remote exec transport requires explicit PTY, stdin, resize, close, and checkpoint support.
Existing `command/exec` and runner process polling do not provide these capabilities.

Terminal types are:

| Type | Fields |
| --- | --- |
| `TerminalIdentity` | `{terminalId, daemonInstanceId, threadId, runnerId, bindingRevision, root}`. |
| `TerminalAttachment` | `{attachmentId, workspaceId, writerLease?, acceptedInputSeq, acceptedResizeSeq, leaseExpiresAt}`. |
| `TerminalCheckpoint` | `{checkpointId, version, engineVersion, nextOffset, resizeOrdinal, columns, rows, total, digest, expiresAt}`. |
| `TerminalResize` | `{ordinal, offset, columns, rows}`. Ordinals increase across the terminal's lifetime, including writer replacement. |
| `TerminalStatus` | `{state:"running"|"exited"|"closed", inputState:"ready"|"uncertain"|"closed", exitCode?, signal?, finalOffset?, finalResizeOrdinal?, outputTruncated?, outputExpiresAt?}`. |

| Method | Parameters | Result |
| --- | --- | --- |
| `dalgona/terminal/open` | `{workspaceId, columns, rows, operationId}` | `{identity, attachment, checkpoint, status}`. |
| `dalgona/terminal/list` | `{workspaceId}` | `{terminals:[{identity, status, writerPresent, leaseExpiresAt}]}`. |
| `dalgona/terminal/attach` | `{workspaceId, terminalId, access:"read"|"write", resumeLease?}` | `{identity, attachment, checkpoint, status}`. |
| `dalgona/terminal/checkpoint/open` | `{terminalId, attachmentId}` | `{checkpoint, status}`. |
| `dalgona/terminal/checkpoint/read` | `{terminalId, attachmentId, checkpointId, offset?, limit?}` | `{offset, next, total, digest, dataBase64}`. |
| `dalgona/terminal/read` | `{terminalId, attachmentId, offset, afterResizeOrdinal, limit?, waitMs?}` | `{offset, next, dataBase64, resizes, resizeOrdinal, status, leaseExpiresAt}`. |
| `dalgona/terminal/write` | `{terminalId, writerLease, inputSeq, dataBase64}` | `{acceptedSeq}`. |
| `dalgona/terminal/resize` | `{terminalId, writerLease, resizeSeq, columns, rows}` | `{acceptedSeq, ordinal, offset, columns, rows}`. |
| `dalgona/terminal/close` | `{terminalId, operationId}` | `{status}`. |
| `dalgona/terminal/detach` | `{terminalId, attachmentId}` | `{status:"detached"|"notFound"}`. |

Open requires the workspace's `terminal` right and an explicit launch action.
The backend selects the configured interactive shell for the execution target.
It starts in the granted workspace root under the existing sandbox policy.
The GUI MUST show the target shell and sandbox state before the launch action.
Shell input is user-authored raw input, not an agent tool request.

One terminal permits at most four attachments, with at most one writer lease among them.
A competing writer receives `busy`. Takeover requires the existing writer's explicit release or lease expiry.
List exposes only terminals matching the authorized thread, runner, root, and binding revision.
Its maximum is sixteen records and it never returns another attachment's lease secret.
Closing a terminal terminates its process group or ConPTY job and revokes writer authority.
Existing attachments remain valid for reading final output under the fixed retention deadline below.
It does not interrupt an unrelated agent turn.
Close allows two seconds for graceful termination, then forces termination and drains output for at most three additional seconds.
Unconfirmed termination returns `unknown_outcome` without claiming that the process ended.
Natural process exit uses the same three-second output-drain limit.
Finalization records `finalOffset`, `finalResizeOrdinal`, `outputTruncated`, and `outputExpiresAt` after draining reaches EOF or its deadline.
`outputTruncated` is true when the drain deadline expires before EOF, and the UI MUST identify that loss.
These final fields remain absent while draining continues. An exited status alone MUST NOT make the client stop reading.

Columns are 20 through 240. Rows are 5 through 120.
The output ring retains the latest 8 MiB of raw PTY bytes.
`read` limits default to 16 KiB and range from one byte through 64 KiB.
`waitMs` is 0 through 1000 and defaults to zero.
Offsets count bytes, including control sequences and incomplete UTF-8 sequences.
The resize ring retains at most 1024 records and 256 KiB of encoded JSON.
Each read returns at most 64 resize records and 16 KiB of encoded resize metadata.

The terminal owner serializes output processing and committed resizes into one order.
A resize's `offset` is the next output byte at its commit point.
The record applies after bytes before that offset and before bytes starting at that offset.
`ordinal` is an `Ordinal`, starting at `"1"`. Zero means no resize has been applied.
Multiple resizes at the same offset MUST remain distinct and ordered. They MUST NOT be coalesced.

`afterResizeOrdinal` identifies the last resize already applied by the reader.
The `resizes` array MUST contain consecutive records after that ordinal, in ordinal order.
Each returned resize offset MUST lie between the requested byte offset and returned `next`, inclusive.
The returned `resizeOrdinal` identifies the last delivered resize, or retains that request value when `resizes` is empty.
The client MUST process returned bytes and resize records by offset, ordering records at the same offset by ordinal.
It MUST then resume with both returned cursors, `next` and `resizeOrdinal`.
A read MUST NOT return bytes at or beyond an undelivered resize's offset.
Reaching the resize-record bound MAY therefore return zero bytes while advancing the resize cursor.
An empty byte payload MUST NOT imply EOF or absence of state changes.
A read MUST advance at least one cursor when retained bytes or resize records are available at its cursor pair.

Every attachment, including read-only attachments, receives this same ordered resize history through reads.
The resize reply confirms its operation but MUST NOT independently change a client's emulator dimensions.
The client applies that resize only at its ordered read position, or through a later checkpoint containing it.
Local viewport geometry MAY change immediately, but it MUST NOT change the replicated terminal grid before that point.

A requested offset before the retained byte ring or a required evicted resize record returns `replay_gap`.
An attached reader obtains a new checkpoint through `checkpoint/open` instead of skipping bytes or allocating another attachment.
Initial attachment still obtains its first checkpoint through `attach`.
Both paths capture terminal state, `nextOffset`, and `resizeOrdinal` atomically with output and resize processing.
Its state reflects all processed bytes before that offset and every resize through that ordinal.
The client starts reads with exactly those two cursors.
The daemon MUST reject inconsistent cursor pairs with `invalid_params` before returning data.
The last applied resize cannot lie after the byte cursor, and an unapplied resize cannot lie before it.
Cursors beyond the terminal's current byte or resize position are invalid.
The ring retains the boundary metadata needed to validate its oldest available cursor pair.

Checkpoint reads default to 64 KiB and permit at most 256 KiB.
The client reassembles at most 8 MiB and verifies the final digest before importing state.
It MUST NOT import a partially downloaded checkpoint.
Checkpoints expire after sixty seconds and belong to their terminal attachment.
Each attachment owns one published checkpoint. The daemon holds at most 128 MiB of checkpoint bytes, including unpublished replacements.
Capacity refusal returns `limit_exceeded` before replacing a usable checkpoint or attachment.
All checkpoint read results remain within the ordinary encoded result budget.

`checkpoint/open` refreshes the calling connection's existing attachment without changing its identity, workspace binding, or access role.
It requires no additional attachment slot and remains available for retained finalized terminals before their fixed output expiry.
A replacement is built within the existing eight MiB checkpoint-body limit and shared checkpoint budget.
The server publishes the replacement atomically, then retires that attachment's previous checkpoint.
An active checkpoint read prevents replacement and returns `busy` without changing the old attachment or checkpoint.
The client MUST finish or cancel its checkpoint reads before refreshing.
Allocation, validation, or budget refusal MUST preserve the previous usable checkpoint and attachment.
Expired attachments retain their normal expiry result. Refresh MUST NOT recreate released state or extend finalized output retention.

The checkpoint body is versioned structured VT state, not executable terminal output.
It contains both grids, scrollback, cursor, saved cursors, modes, margins, tabs, current attributes, links, and parser continuation.
The state also includes pending UTF-8 bytes and incomplete escape sequence bytes.
Its digest covers the complete canonical UTF-8 JSON body.
The entire body is limited to 8 MiB encoded JSON.
Scrollback is limited to 1 MiB and 10000 rows, whichever arrives first.
Eviction removes only the oldest completed scrollback rows.

The body schema is `VTStateV1`:

| Member | Contract |
| --- | --- |
| `primary`, `alternate` | Screen objects containing `rows`, `cursor`, `savedCursor`, and `margins`. |
| `activeScreen` | Exactly `primary` or `alternate`. |
| `scrollback` | Oldest-first row objects within the scrollback bounds. |
| `styles` | Indexed style records containing colors, underline kind, and text attribute flags. |
| `links` | Indexed `{uri, id}` records under the document hyperlink allowlist. |
| `currentStyle` | Valid index into `styles`. |
| `modes` | Enabled ANSI and DEC mode numbers, plus saved mode values. |
| `tabStops` | Sorted unique zero-based columns below `columns`. |
| `charsets` | G0 through G3 designation, active GL and GR, and any pending single shift. |
| `cursorStyle`, `cursorVisible` | Supported cursor shape and visibility. |
| `parserStateBase64` | At most 64 KiB of versioned parser continuation from the pinned checkpoint adapter. |

A row is `{cells, softWrapped}` and contains exactly `columns` cells.
A cell is `{text, width, style, link?}`.
Width is zero for a wide-character continuation, otherwise one or two.
Style and link indices MUST reference existing entries.
A cursor is `{row, column, wrapPending}` with positions inside its screen.
A saved cursor additionally records its style, origin mode, and character set state.
Margins are inclusive `{top, bottom, left, right}` positions inside the screen.
Colors are `{kind:"default"}`, `{kind:"indexed", index}`, or `{kind:"rgb", red, green, blue}`.
Color components and indexed values are integers from zero through 255.
Mode numbers and flags are validated against the pinned engine's supported set.

The adapter owns the exact parser continuation encoding for checkpoint version one.
It MUST reject unknown flags, impossible cell geometry, unsupported parser state, and invalid UTF-8 outside preserved partial parser bytes.
The implementation MUST publish independent checkpoint fixtures covering every supported parser state before advertising checkpoint version one.
It cannot serialize only the visible grid and claim equivalent continuation.

The daemon and GUI MUST use the same pinned VT engine and checkpoint schema.
The checkpoint adapter belongs outside the GUI package so the daemon never imports Gio.
The current `x/vt` dependency does not supply a complete public checkpoint import/export contract.
Implementing and validating that adapter is required work.
A checkpoint version mismatch returns `incompatible` before input is accepted.

The daemon's authoritative VT processor alone generates automatic terminal protocol replies.
This includes device attributes and status-report responses.
It sends each response once while consuming live PTY output.
GUI processors suppress automatic replies during checkpoint import and live rendering.
Reader attachments never write protocol responses.
Writer leases authorize user input and user resizing only.
Daemon-generated protocol responses use a separate internal output path without consuming user input sequence numbers.
The terminal owner serializes that path with user input before either reaches PTY stdin.

Terminal input is limited to 16 KiB per write.
`inputSeq` starts at `"1"` and increases by one for the writer lease.
The server retains the last 256 input digests and acknowledgments.
It reserves each sequence and digest as pending before dispatching any byte.
An exact duplicate of a pending write returns `busy` without another write.
Exact duplicates return the prior acknowledgment without writing again.
An older unretained sequence returns `expired`. A sequence gap or changed duplicate returns `operation_conflict`.
The GUI allows at most 64 unacknowledged input writes.

`acceptedInputSeq` advances only after every byte in that sequence is proven written.
A partial write or unprovable remote acknowledgment returns `unknown_outcome` and sets the terminal's `inputState` to `uncertain`.
The server retains that outcome and MUST NOT replay the consumed prefix or dispatch queued or new input.
The same rule applies when an automatic terminal reply is only partly written or has an unprovable outcome.
Output reading and explicit close remain available.
Attach, read, and list expose that terminal-wide state. Reconnect and a different writer lease cannot clear it.
The UI offers Stop and an explicit new terminal action without resending the uncertain input.
Exited and closed terminals expose `inputState:"closed"`.

Resize uses an independent increasing `resizeSeq` and the same duplicate rules.
That writer sequence is separate from the terminal-wide resize ordinal.
An acknowledged resize changes both the PTY and authoritative emulator dimensions and appends exactly one resize record before replying.
Output processing MUST NOT cross that commit point while only one side has changed.
An exact duplicate returns the original acknowledgment and MUST NOT append another record.
An unconfirmed PTY resize returns `unknown_outcome` and sets `inputState:"uncertain"` without publishing a successful resize record.
The authoritative emulator retains its last committed dimensions while output remains readable.
Further writes and resizes MUST fail until the user explicitly stops that uncertain terminal.

Read, write, resize, attach, and checkpoint operations renew a 30-second connection lease.
An otherwise idle visible terminal sends a read at least every ten seconds.
Transport loss preserves the terminal for 30 seconds after its last successful lease renewal.
The writer lease remains reserved during that interval.
Reattachment with its secret in `resumeLease` restores the same sequence counters and ownership.
The GUI keeps that secret in memory only and discards it after expiry.
It reconciles pending input against `acceptedInputSeq` before retrying an exact retained sequence.
It MUST NOT resend uncertain input through a newly allocated lease.
The running session ends when its final connection lease expires.
Detaching the last attachment leaves its previous lease deadline as the stop deadline and MUST NOT extend that deadline.

Hiding a terminal panel does not release the application's attachment.
The application continues its ten-second lease reads while retaining that terminal.
Explicit Stop and the window-exit decision perform close.
Attach with `access:"read"` and the current `resumeLease` releases writer ownership while retaining a reader attachment.
Detach releases only the named attachment, its checkpoint, and any writer lease it owns.
It MUST NOT close another attachment or report that the process exited.
Repeating detach is harmless. A foreign attachment MUST NOT be detached or exposed.

Terminals do not survive daemon restart.
Recovered identifiers return `expired` and never attach to a different process.
Runner loss reports unavailable or exited status without launching a replacement shell.
Final output and status remain readable for sixty seconds after finalization, including after explicit close.
This fixed `outputExpiresAt` deadline MUST NOT be extended by reads, attach, or repeated close.
Read leases on finalized terminals are capped by that deadline.
Fresh authorized attachments MAY inspect the retained terminal with `access:"read"` before expiry. Writer attachment MUST fail.
The client MUST read until both `finalOffset` and `finalResizeOrdinal` are reached before declaring final output collected.
Byte-ring eviction still reports `replay_gap`. A final checkpoint can recover retained screen state, not discarded raw history.
Finalized terminals count toward the existing terminal-session and memory bounds until their retained state is released.
Detaching the last attachment after finalization releases that retained state early.
At retention expiry, the daemon releases every remaining attachment, checkpoint, resize record, and output byte.
An expired identifier MUST NOT reopen a process or return data from another terminal.

Reattachment requires a fresh workspace handle with the necessary terminal right.
Terminal identifiers alone are insufficient authority.
The implementation MUST bind each attachment to its authenticated endpoint and thread.

Terminal output and input are unmasked raw data.
They MUST NOT enter transcript journals, telemetry, or failure excerpts automatically.
Copying selected terminal text requires an explicit user action.

## R13. Remote and runner management

Client-owned endpoint operations remain in the shared lifecycle package.
They are `Endpoints`, `SaveRemote`, `CloudUp`, `CloudPull`, and `CloudDown`.
Their target is the locally selected remote configuration, not the current conversation daemon.
They return typed progress and final outcomes instead of parsing CLI output.

The existing product methods retain their names:

| Method | Native parameters | Native result |
| --- | --- | --- |
| `dalgona/remote/pull` | `{threadId, dalgonaOperationId}` | `{exitCode, output, changedPaths, conflictPaths, bindingRevision}`. |
| `dalgona/remote/gc` | `{yes, dalgonaOperationId}` | `{exitCode, output, candidates, destroyed, failures}`. |

Structured fields apply only after `runnerManagement:1` negotiation.
The existing `exitCode` and `output` remain available.
`output` is masked and limited to 64 KiB in native results.
Path lists contain at most 1000 entries with an explicit `truncated` flag when their display summary exceeds that count.
Truncating the display summary MUST NOT change the reported mutation outcome.

`gc` with `yes:false` produces a non-destructive candidate preview.
Destructive execution requires a confirmation bound to that preview's ledger revision.
The native request then includes `previewRevision` and `yes:true`.
A changed ledger returns `stale_revision` before any destruction.
Candidate records contain `{boxId, service, threadIds, reason}`.
Failure records contain `{boxId, kind}` without credentials.

Add `dalgona/remote/status {threadId?, cursor?, limit?}` for runner inventory and binding state.
Its result is `{runners, bindings, ledgerRevision}`.
Runner records contain `{name, service, available, capabilities}`.
Binding records contain `{threadId, runnerId, boxId?, sharedThreadIds, state, bindingRevision}`.
The default is 100 records. The maximum is 500 records per page across both collections.
Pagination uses the opaque `cursor` parameter and `nextCursor` result.

Native pull progress uses `dalgona/remote/progress {operationId, phase, completed, total?}`.
`phase` is `connecting`, `reading`, `merging`, `publishing`, or `finished`.
Counts use decimal strings. An unknown total is omitted.
Notifications are limited to four per second and do not replace the final mutation result.
Client-owned cloud operations publish the same typed progress through their local operation owner.

Pull preserves the existing three-way merge and conflict behavior.
The UI shows its local destination domain before execution.
Archive confirmation discloses whether the last shared thread box will be destroyed.
Cloud down separately discloses loss of the remote daemon and every thread stored there.

## R14. Diagnostics and failure records

The new `dalgona/diagnostics/run {online:false|true}` returns `{rows, exitCode}`.
Each row is `{status:"ok"|"warn"|"fail", check, detail}`.
Rows preserve the order and meaning in [doctor.md](doctor.md).
Each detail is masked, control-cleaned, and limited to 400 characters.

Online mode requires an explicit action and sends only the existing bounded model-list probes.
Each probe has a five-second deadline, no retry, no account rotation, and no token refresh.
Offline mode makes no provider request.
Neither mode starts MCP, runs sandbox commands, repairs files, or starts a daemon.

The local disconnected panel uses `client.Diagnose` with the same row engine.
It MUST remain usable when startup itself fails.
Connected remote diagnostics describe the remote endpoint, never the GUI host.
Unavailable remote diagnostics cannot be replaced silently by local results.

The new `dalgona/failures/list {cursor?, limit?}` returns `{records, nextCursor?}`.
It lists masked metadata summaries in reverse chronological order.
The default limit is 50. The maximum is 100.
The cursor freezes a listing boundary and expires after five minutes.

The new `dalgona/failures/read {id}` returns the existing public `Record` schema.
Its masking, classification, bounds, and content-free fallback follow [failures.md](../../dal/spec/failures.md).
It MUST NOT include the local correlation sidecar or private replay inputs.
Downloading a record exports only those same masked public fields.

## R15. Consented reporting

The new `dalgona/reporting/read {method?}` returns:

```json
{
  "method":"app",
  "enabled":false,
  "available":true,
  "consentText":"exact substituted text",
  "textHash":"sha256",
  "consentReceipt":null,
  "authorization":"missing",
  "restriction":null,
  "queueCount":0,
  "uncertainCount":0
}
```

`method` is `app` or `gh` and defaults to the configured method.
`consentReceipt` is null or `{schemaVersion:1, time, method, textHash}`.
`authorization` is `ready`, `missing`, `expired`, or `unavailable`.
`restriction` is null or `{kind, until?, message}` using the reporter's existing bounded state.

The new `dalgona/reporting/enable` accepts `{method, textHash, confirmation, operationId}`.
`confirmation` MUST equal the exact string `yes`.
The hash MUST match the current substituted consent text and selected method.
The daemon writes the receipt durably before enabling configuration.
A write failure leaves reporting off.
The result returns the stored `consentReceipt` and current authorization state.

The native typed confirmation explicitly extends the existing terminal-only consent presentation.
It preserves the requirement for a deliberate person action and exact text.
Checkbox defaults, generic settings writes, project configuration, and environment variables cannot opt in.
Changing authentication methods requires a new receipt and cannot inherit old consent.

The new `dalgona/reporting/disable {operationId}` disables first and removes consent, App credentials, and queued excerpts.
It preserves local records, receipts, counters, and restriction deadlines.
The result is `{enabled:false, queuedReportsDeleted:true, publishedIssuesDeleted:false}`.

Reporter login uses R09 with the reporter target.
App mode retains its separate client identifier, device flow, repository checks, and restricted permission.
Gh mode uses only the existing explicit `gh auth token --hostname github.com --no-refresh` path.
The native client MUST display that acquisition notice before enabling gh mode.
There is no fallback, login, refresh, or inherited provider credential.

All queue, privacy, rate, restriction, and uncertain-POST rules remain those of [report.md](report.md).
Overridden homes and testing modes remain ineligible for remote reporting.
Neither restarting nor changing authentication clears an HTTP restriction.
The panel MUST show uncertain reports without offering an automatic duplicate submission.

## R16. Deadlines, cancellation, and verification

Ordinary metadata reads have a ten-second deadline.
History and workspace pages have a thirty-second deadline.
PTY reads use their one-second maximum wait within a five-second request deadline.
Authentication uses its flow-specific limits.
Long runner operations report bounded progress and accept cancellation without inventing a successful rollback.

Native connections support the new notification `$/cancelRequest {id}` after `requestCancellation:1` negotiation.
`id` preserves the exact original JSON-RPC identifier type and value.
The server maps it only to an active request on that connection.
Unknown or completed identifiers are ignored.
Cancellation requests stop cancellable work and do not imply rollback of a dispatched effect.
The existing `rpc.Conn` currently cancels local waiting only when a call context ends.
The implementation MUST add negotiated server cancellation rather than claim that behavior already exists.

Every worker inherits its owning connection context unless the contract explicitly grants a reconnect lease.
Cancellation before effect returns `cancelled`.
Cancellation after dispatch follows R07 and may produce `unknown_outcome`.
No cancellation response may claim that a committed filesystem write, credential change, or cloud action was undone.

The native release MUST pass independent fixtures for every method family and all error variants.
Malformed sizes, foreign handles, cross-thread image reads, and stale binding revisions MUST fail before their effects.
Stream tests MUST cover snapshot races, coalescing boundaries, pending requests, and disconnect during mutation dispatch.
History tests MUST prove bounded reads on long threads and continuation within oversized turns.
PTY tests MUST split UTF-8 and escape sequences at every checkpoint boundary.
Auth tests MUST cover every provider and MCP resource mismatch without exposing secrets.
Reporting tests MUST prove receipt-before-enable ordering and unchanged restriction latches.

The implementation MUST update the affected core topic specifications in the same behavior change.
This native document does not silently redefine existing daemon, OAuth, runner, or reporter behavior for legacy clients.
