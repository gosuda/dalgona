# Approvals

This specification defines the approval gate of the agent core. Approvals are automated. User prompts occur only in approval mode `ask`.

The modes are ordered `yolo` < `auto` < `ask`. A protocol client MAY raise the mode of a turn. A per-turn approval mode MUST NOT lower the thread mode: the effective mode is the stricter of the thread mode and the turn mode. A thread in `ask` therefore keeps user prompts when a client sends `on-request` for a turn.

## Journal records

Before a call runs, the gate MUST journal `approval.requested` with `request_id`, `call_id`, `summary`, and the call `class`. The gate MUST then journal `approval.resolved` with `decision`, `by`, `reason`, and, for decider verdicts, `scores`. A call MUST NOT reach `tool.started` without an `accept` or `accept_session` resolution.

`by` MUST be one of `mode:<mode>`, `rule:<name>`, `decider:<model>`, `user`, `grant:<prefix>`, or the `By` value of a hook.

A declined call MUST complete with `tool.completed`, `ok=false`, and the decline reason as its output. It MUST NOT be marked `skipped`. Later calls in the same assistant message MUST still run. After a cancel decision on an in-band call, every later call in that turn MUST complete as declined and MUST NOT run.

Calls that fail before the gate MUST complete through a declined resolution:

- An unknown tool: `Tool <name> not found`, by `mode:<mode>`.
- Arguments that `tool.Repair` rejects: `invalid arguments for <name>: <error>`, by `mode:<mode>`.
- The loop guard: `repeated call: this exact call already ran 4 times; change approach`, by `rule:loop-guard`. The key is the tool name plus the arguments in RFC 8785 canonical form. Four earlier `tool.started` records with the same key trigger the guard. Counting stops at the most recent `user.message`. Counting also stops at the most recent `tool.started` record with a different key when that record is class `edit` or `exec` and completed with `ok=true`. A failed or unfinished state-changing call MUST NOT reset the count. Other calls between repeats MUST NOT reset the count.

## Gate order

The gate MUST run these steps in order and MUST stop at the first verdict.

1. Plugin hooks. Every plugin `tool_call` hook runs as a durable script run `<call_id>/hook:<file>:<fn>`. Every hook MUST run. The first `deny` declines and otherwise the first `allow` accepts, both with `by` = `hook:<file>:<fn>`. A hook failure MUST be logged. A hook failure MUST count as no opinion for that hook only. Later hooks MUST still run. A `turn_end` hook run `<turn_id>/turn_end/hook:<file>:<fn>` MUST run before `turn.completed`, within 30 s.
2. Mode `yolo`. Accept by `mode:yolo`. `exec` then runs without the harness sandbox.
3. Mechanical allow, modes `auto` and `ask`. Classes `read`, `agent`, and `script` accept by `mode:<mode>`. In `auto`, class `edit` accepts by `mode:auto` when every planned path is inside the writable roots. In `ask`, class `edit` goes to step 7.
4. Static rules for `exec` commands. For `remote` calls, only the semantic deny rules apply. They check each decoded string value of the JSON arguments. Shell-syntax rules never see the raw JSON. A match declines by `rule:<name>` with the reason `declined by rule <name>: <command part>`. The decider MUST NOT see the call.
5. Sandbox choice. Steps 1 to 4 run first. A hook or static rule decides an exec call before this step. This step applies to `exec` calls when the harness sandbox is missing. It applies only to threads whose sandbox is `workspace-write` in modes `auto` and `ask`. The thread asks the user once, and a recorded choice holds for the thread. `Accept` sends the call to the user, without thread grants. `Dangerously accept` runs the call without confinement under the thread's approval mode. Without a subscribed client, the call declines with `declined: no client could choose how to run without a sandbox (<cause>); set sandbox = "off" to run without one`.
6. Decider, mode `auto`. The gate sends an `Action` to the configured `Decider`. A missing decider declines with `declined by decider: no decider is configured`. A decider error declines with `declined by decider: <error>`.
7. User, mode `ask`. A matching thread grant accepts by `grant:<prefix>`. With no subscribed client, the gate declines with `declined: no client could approve this call`. Otherwise the thread publishes an `UpdateApproval` and waits for `Thread.Answer`. The prompt summary names the thread working directory and, for an escalated `exec` call, that the call runs unsandboxed. For an `exec` call the prompt carries the exact command text. The app-server MUST show that text verbatim, not as a reassembled form. `accept` and `accept_session` accept by `user`. `decline` declines with `declined by user`. `cancel` declines with `cancelled by user` and interrupts the turn. An interrupt during the wait resolves the request as `cancel` by `user`.

An advisor thread MUST NOT wait for a user decision. In mode `ask`, it MUST use step 6 instead of step 7.

A `-p` turn answers every approval request with `decline` and prints a notice to stderr.

Subagent threads change step 7. A subagent MUST also publish its `UpdateApproval` to the parent's subscribers, with `Update.Thread` set to the subagent id. Only parent subscribers count as clients for a subagent. With no parent subscriber, the gate declines with `declined: no client could approve this call`. When the request resolves, the subagent MUST publish the same `UpdateApproval` with `Resolved=true` to the parent's subscribers. The app-server MUST then send `serverRequest/resolved` with the subagent `threadId` and the `requestId`. When the last parent connection leaves, the app-server MUST decline the pending subagent request. A connection that watches both threads MUST send each approval request id once. A repeated delivery of the same request id MUST NOT send a second request.

A client error on an approval request MUST mean that the client cannot answer. The app-server MUST decline the request when every attached subscriber has reported that it cannot answer. The app-server MUST decline the request when the last subscriber leaves. For a subagent request, the attached subscribers are the parent's subscribers. Otherwise the request MUST stay open for the other subscribers. The first answer MUST win.

Writable roots are the thread working directory, `os.TempDir()`, `os.UserCacheDir()`, and `sandbox_writable_roots`. Each root and path MUST pass through `filepath.EvalSymlinks` for its longest existing prefix.

## Decider adapter

`NewDecider(client, model, threshold, product)` adapts a `decide.Client` to the `Decider` seam.

- The state MUST be a JSON object with `commands`, `project`, `task`, `agent`, `sandboxed`, and, when present, `justification`. `commands` holds the `rules.Split` parts for `exec`, `<tool> <args>` for `remote`, and `write <path>` for each edit path outside the roots. `task` holds the last user message of the active path, cut to 2000 bytes. `agent` is `dal` or `dalgona`.
- The adapter MUST ask the `noul` question `reversible`. It MUST ask `serves_task` only when `task` is not empty.
- The adapter MUST allow only when every asked score is at or above the threshold.
- A decline MUST use the reason `declined by decider <model>: reversible=<x> serves_task=<y> (threshold <t>)`. The `serves_task` part MUST be absent when the question was not asked. Numbers MUST use the shortest decimal form.
- An allowed verdict MUST use the same reason form, with `allowed` in place of `declined`.
- An error, a timeout of 8 s, or a missing or out-of-range answer MUST decline with `declined by decider <model>: <error>`.
- An escalated `exec` call (`escalate=true`) MUST be decided with `sandboxed=false` and its justification. When allowed, it runs without the harness sandbox.

`NewFactoryDecider` MUST resolve its decide client for the thread being judged on each action, then score through the same `Decide` path.
A `ClientFactory` MUST return the decide client and model for the thread it receives.

## Thread grants

A user `accept_session` resolution MUST create a thread grant once the fold knows the call class from `approval.requested`. When the resolution is journaled before the class is known, the fold carries the grant prefix forward. The fold creates the thread grant from the later `tool.started` record.

- `exec`: the thread grant is the first two argv words from `rules.GrantPrefix`, with a flag in second position kept. A compound command MUST NOT create a thread grant. The thread grant records whether the accepted call was escalated, and a later command matches only when escalation agrees and `rules.MatchesGrant` holds.
- `edit` and `remote`: the thread grant is `tool:<name>`.
- Other classes MUST NOT create thread grants.
- A `grant:<prefix>` resolution MUST NOT create a new thread grant.

Thread grants MUST fold over the full journal, so they survive a revert, a leaf move, and a restart.

A sandbox choice of `Accept` MUST route every exec call of the thread that reaches the choice step to the human approval path. No thread grant MUST apply.
