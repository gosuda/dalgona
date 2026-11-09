# Approvals

This specification defines the approval gate of the agent core. Approvals are automated. Human prompts occur only in approval mode `ask`.

The modes are ordered `yolo` < `auto` < `ask`. A protocol client MAY raise the mode of a turn. A per-turn approval policy MUST NOT lower the thread mode: the effective mode is the stricter of the thread mode and the turn mode. A thread in `ask` therefore keeps human prompts when a client sends `on-request` for a turn.

## Journal records

Before a call runs, the gate MUST journal `approval.requested` with `request_id`, `call_id`, `summary`, and the call `class`. The gate MUST then journal `approval.resolved` with `decision`, `by`, `reason`, and, for decider verdicts, `scores`. A call MUST NOT reach `tool.started` without an `accept` or `accept_session` resolution.

`by` MUST be one of `mode:<mode>`, `rule:<name>`, `decider:<model>`, `user`, `grant:<prefix>`, or the `By` value of a hook.

A declined call MUST complete with `tool.completed`, `ok=false`, and the decline reason as its output. It MUST NOT be marked `skipped`. Later calls in the same assistant message MUST still run.

Calls that fail before the gate MUST complete through a declined resolution:

- An unknown tool: `Tool <name> not found`, by `mode:<mode>`.
- Arguments that `tool.Repair` rejects: `invalid arguments for <name>: <error>`, by `mode:<mode>`.
- The loop guard: `repeated call: this exact call already ran 4 times; change approach`, by `rule:loop-guard`. The key is the tool name plus the arguments in RFC 8785 canonical form. Four earlier `tool.started` records with the same key trigger the guard. Counting stops at the most recent `user.message` and at the most recent `tool.started` record with a different key in class `edit` or `exec`; other calls between repeats do not reset the count.

## Gate order

The gate MUST run these steps in order and MUST stop at the first verdict.

1. Plugin hooks. Every plugin `tool_call` hook runs as a durable script run `<call_id>/hook:<file>:<fn>`. Every hook MUST run. The first `deny` declines and otherwise the first `allow` accepts, both with `by` = `hook:<file>:<fn>`. A hook failure MUST be logged and MUST count as no opinion for that hook only; later hooks MUST still run. A `turn_end` hook run `<turn_id>/turn_end/hook:<file>:<fn>` MUST run before `turn.completed`, within 30 s.
2. Mode `yolo`. Accept by `mode:yolo`. `exec` then runs without the harness sandbox.
3. Mechanical allow, modes `auto` and `ask`. Classes `read`, `agent`, and `script` accept by `mode:<mode>`. In `auto`, class `edit` accepts by `mode:auto` when every planned path is inside the writable roots. In `ask`, class `edit` goes to step 6.
4. Static rules for `exec` commands. For `remote` calls, only the semantic deny rules apply, checked against each decoded string value of the JSON arguments; shell-syntax rules never see the raw JSON. A match declines by `rule:<name>` with the reason `declined by rule <name>: <command part>`. The decider MUST NOT see the call.
5. Decider, mode `auto`. The gate sends an `Action` to the configured `Decider`. A missing decider declines with `declined by decider: no decider is configured`. A decider error declines with `declined by decider: <error>`.
6. Human, mode `ask`. A matching session grant accepts by `grant:<prefix>`. With no subscribed client, the gate declines with `declined: no client could approve this call`. Otherwise the thread publishes an `UpdateApproval` and waits for `Thread.Answer`. The prompt summary names the thread working directory and, for an escalated `exec` call, that the call runs unsandboxed. For an `exec` call the prompt carries the exact command text, and the app-server MUST show that text verbatim rather than a reassembled form. `accept` and `accept_session` accept by `user`. `decline` declines with `declined by user`. `cancel` declines with `cancelled by user` and interrupts the turn. An interrupt during the wait resolves the request as `cancel` by `user`.

Child threads change step 6. A child of a loaded parent, other than an advisor, MUST also publish its `UpdateApproval` to the parent's subscribers, with `Update.Thread` set to the child id. Only parent subscribers count as clients for such a child. With no parent subscriber, the gate declines with `declined: no client could approve this call`. When the request resolves, the child MUST publish the same `UpdateApproval` with `Resolved=true` to the parent's subscribers. The app-server MUST then send `serverRequest/resolved` with the child `threadId` and the `requestId`. When the last parent connection leaves, the app-server MUST decline the pending child request. A connection watching both threads MUST send each approval request id once; a repeated delivery of the same request id MUST NOT send a second request.

Writable roots are the thread working directory, `os.TempDir()`, `os.UserCacheDir()`, and `sandbox_writable_roots`. Each root and path MUST pass through `filepath.EvalSymlinks` for its longest existing prefix.

## Decider adapter

`NewDecider(client, model, threshold, product)` adapts a `decide.Client` to the `Decider` seam.

- The state MUST be a JSON object with `commands`, `project`, `task`, `agent`, `sandboxed`, and, when present, `justification`. `commands` holds the `rules.Split` parts for `exec`, `<tool> <args>` for `remote`, and `write <path>` for each edit path outside the roots. `task` holds the last user message of the active path, cut to 2000 bytes. `agent` is `dal` or `dalgona`.
- The adapter MUST ask the `noul` question `reversible`. It MUST ask `serves_task` only when `task` is not empty.
- The adapter MUST allow only when every asked score is at or above the threshold.
- A decline MUST use the reason `declined by decider <model>: reversible=<x> serves_task=<y> (threshold <t>)`. The `serves_task` part MUST be absent when the question was not asked. Numbers MUST use the shortest decimal form.
- An error, a timeout of 8 s, or a missing or out-of-range answer MUST decline with `declined by decider <model>: <error>`.
- An escalated `exec` call (`escalate=true`) MUST be decided with `sandboxed=false` and its justification. When allowed, it runs without the harness sandbox.

## Session grants

A user `accept_session` resolution MUST create a grant once the fold knows the call class from `approval.requested`. When the resolution is journaled before the class is known, the fold carries the grant prefix forward and creates the grant from the later `tool.started` record.

- `exec`: the grant is the first two argv words from `rules.GrantPrefix`, with a flag in second position kept. A compound command MUST NOT create a grant. The grant records whether the accepted call was escalated, and a later command matches only when escalation agrees and `rules.MatchesGrant` holds.
- `edit` and `remote`: the grant is `tool:<name>`.
- Other classes MUST NOT create grants.
- A `grant:<prefix>` resolution MUST NOT create a new grant.

Grants MUST fold over the full journal, so they survive a revert, a leaf move, and a restart.
