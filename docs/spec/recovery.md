# Recovery

This specification defines how the agent core recovers a session after a halt, Ctrl-C, SIGKILL, or host reboot. The journal is the authority. Recovery MUST read only the journal, the blob store, the runner, and the workspace files.

## When recovery runs

- dalgona MUST call `Host.Recover` at daemon start. It MUST recover every session that has an open turn.
- dal MUST recover a session when `Host.Resume` opens it.
- `Host.Recover` MUST NOT resume advisor child sessions. The advisor runtime MUST resume them with `Host.ResumeWith` and the advisor tools and prompt.
- A damaged session MUST open read-only up to its last good sequence. Recovery MUST NOT write to it.

## Folded state

The host MUST replay the journal through the event fold before it acts. Grants, read snapshots, runner bindings, advisor notes, and completed turns MUST fold over the full journal. Model context MUST use only the active path.

Read snapshots MUST be restored from their blobs in epoch and revision order. A snapshot that cannot be restored MUST be logged. It MUST NOT stop recovery.

## Rules

Recovery MUST walk the tool calls of the last `assistant.message` of the open turn, in call order. Each call MUST match exactly one row.

|State at crash|Recovery|
|---|---|
|Turn open, no call pending|Rebuild the request from the folded active path and call the model again. Streamed text that was not journaled is discarded.|
|Call with `tool.completed`|No action.|
|`approval.requested` without `approval.resolved`|Decide again. `auto`: static rules, then the decider. `ask`: ask the clients again; with no subscribed client, decline with `declined: no client could approve this call`. The existing request id MUST be reused.|
|`approval.resolved` with `decline` or `cancel`, no `tool.completed`|Journal `tool.completed` with `ok=false`, `recovered=none`, and the decline reason as output.|
|`approval.resolved` with `accept` or `accept_session`, no `tool.started`|Journal `tool.completed` with `ok=false`, `recovered=skipped`, output `skipped: the host restarted before this call ran`. The call MUST NOT run. A session grant from `accept_session` stays in force because the fold records it at `approval.resolved`.|
|Assistant call with no approval and no `tool.started`|Same as the previous row: `recovered=skipped`, output `skipped: the host restarted before this call ran`.|
|`tool.started`, class `read`|Run the call again. Journal `recovered=replayed`.|
|`tool.started`, class `edit`, no `files`|Plan and apply the edit again. Journal `recovered=replayed`.|
|`tool.started`, class `edit`, every file sha equals `post_sha`|Journal `ok=true`, `recovered=replayed`, output `the edit was applied before the host restarted`.|
|`tool.started`, class `edit`, every file sha equals `pre_sha`|Plan and apply the edit again. Journal `recovered=replayed`.|
|`tool.started`, class `edit`, any other mix|Restore each file whose sha equals `post_sha` from its `pre_blob`. Remove the file when `pre_sha` is empty. Journal `ok=false`, `recovered=interrupted`, output `interrupted: files changed during recovery: <paths>`. `<paths>` lists every file whose sha differed from `pre_sha`, joined with `, `.|
|`tool.started`, class `exec`, exit recorded|Complete with the recorded output, projected from the tail, and the exit code. Journal `recovered=replayed`.|
|`tool.started`, class `exec`, process alive|Wait through the exec tool. `runner.Start` with the same call id MUST be a no-op. Journal `recovered=replayed`.|
|`tool.started`, class `exec`, process gone without exit|Complete with the output read so far and the line `interrupted: the host restarted while this command ran; it may have partly run`. Journal `ok=false`, `recovered=interrupted`.|
|`tool.started`, class `exec`, no run record|Journal `ok=false`, `recovered=interrupted`, output `interrupted: the host restarted while this command ran; it may have partly run`. The command MUST NOT run again.|
|`tool.started`, class `agent` or `script`|Call the tool again with the same call id. The tool MUST replay from its own journal records. Journal `recovered=replayed`.|
|`tool.started`, class `remote`|Journal `ok=false`, `recovered=interrupted`, output `interrupted: the tool may have partly run`. The call MUST NOT run again.|
|`tool.started` for a tool that the thread no longer has|Same as class `remote`.|

Sha values MUST be SHA-256 of the file bytes, as lowercase hex. An absent file MUST have the empty sha.

After the calls are complete, recovery MUST continue the turn loop. The next model request MUST carry a result for every call.

## Failure behavior

- A journal append failure during recovery MUST end the turn with status `failed` and the error text.
- An interrupt during recovery MUST end the turn with status `interrupted`. Unfinished calls MUST complete as `skipped` or `interrupted` before `turn.completed`.
- Recovery MUST NOT drop or rewrite a committed event.
