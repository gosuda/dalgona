# Orchestration

The orchestration extension coordinates background jobs, monitored output, goals, and child-agent workflows. Use `agents` to run, wait for, cancel, or list workflows. Use `monitor` to watch matching output from a background job. Use `create_goal`, `update_goal`, and `get_goal` with `/goal` to manage a durable session goal. `/continuation` controls automatic turns, and `/abort` cancels active orchestration work. Automatic reminders are delivered only when the session is ready; child reports are claims, not proof.

## Agents, jobs, cancel, and shutdown

Each `agents` run and each background job belongs to one session and uses a bounded queue. `cancel` names a job or a turn and stops it and its descendants through one cancellation tree. Host shutdown closes every open session and stops timers, child sessions, processes, and background jobs before the process exits.

When a workflow exceeds its shutdown grace period, dalgona leaves its existing worktrees in place and records their recovery paths in each task's `retained.json`. Session close waits for this recovery step. If saving a recovery record fails, dalgona reports the failure and leaves the worktree on disk.

Admission checks active runs and the remaining session quota before starting children. `max_runs` defaults to 16 under `[plugin.orchestration.agents]`; `agents_per_session` has a fixed ceiling of 1024. When a limit is reached, wait for a run to end, cancel one, or start a smaller run.

Subagents cannot start subagents. They see only the explicitly listed tools (default `read` and `search`) and cannot ask the user. Modifying tools require worktree isolation: `patch` under approval mode `edits` or `all`, and `exec` under `all`. Without a configured host data root, worktree steps refuse to start; they do not run in the main checkout.

Each child calls `orchestration__report` once with status `done`, `blocked`, or `failed` and 1 to 16,000 characters of report text. Empty and duplicate reports are refused. Rebuild the promised scope, inspect the changed files, and run the checks before trusting a report.

## Wakes

A wake starts a turn without a user prompt. A session accepts 20 wake-started turns in a row; the 21st wake is refused and journaled, and a user prompt resets the count. A wake that reached the session is never repeated: when the host does not confirm the job reports it carried, the owner sees a delivery notice and only the acknowledgement is retried. Other delivery-poll failures are reported once per cause.

## Scope, budget, and deadline

A scope caps concurrent model handles with a limit; handles past the limit wait in FIFO order. Its budget bounds requests, tokens, wall time, and cost; usage rolls up through nested scopes to the parent ledger. Admission fails when the budget is exhausted or a USD-budgeted model has no known price. An `on_error` policy decides what one failed handle does to its siblings. A scope created inside a hook is cancelled when the hook deadline passes; there is no separate timeout setting.

## Mailbox

Sessions in one tree exchange messages through a journal-backed mailbox with per-pair FIFO order and cursor-based reads. Each message carries a mode: `aside` delivers without steering the current turn, `steer` steers the running turn, and `next_turn` queues for the recipient's next turn. A send returns a receipt naming the outcome: delivered, buffered, full, or gone. Reading the mailbox does not consume messages; a full waiting queue returns `Full`, and an out-of-tree or finished recipient returns `Gone`.

## Synthetic models

A synthetic model is a plugin-defined model id whose handler emits a normal event stream and can forward to another model. Its usage is journaled against the synthetic route. A handler may bind private tools that exist only for that call. Only the handler may forward, and only once: a second forward is refused. Private tool calls run inside the handler and never reach the session, a private tool cannot shadow a session tool, a cycle is refused, and a chain of more than four routes is refused.

## Constants

| Constant | Value |
|---|---|
| Loop guard window | 64 records |
| Identical run threshold | 3 |
| Notices before block | 2 |
| Blocked calls before hard stop | 3 |
| Similar run threshold | 5 |
| Similarity threshold | 0.85 |
| Cycle periods | 2..=6 |
| Cycle repetitions | 3 |
| Notice escalation factor | 2 |
| Silence threshold | 600 s |
| Sleep-wait threshold | 10 s |
| Sleep-wait loop threshold | 2 s |
| Wrapper unwrap depth | 3 |
| Sleep-wait foreground window | 5 s |
| Monitor coalesce | 2000 ms, clamp 1..=60000 |
| Monitor rate limit | 5000 ms, clamp 1..=3600000 |
| Monitor lines per injection | 50, clamp 1..=200 |
| Monitor characters per injection | 4096, clamp 512..=16384 |
| Monitor queue overhead | 512 chars |
| Monitor wake budget | 5, clamp 1..=100 |
| Monitor fire budget | 200 per 24 h |
| Monitor line cap | 65536 chars |
| Live monitors per session | 16 |
| Goal consecutive cap | 8 |
| Goal stall toolless threshold | 3 |
| Goal repetition hash streak | 3 |
| Goal length recovery limit | 1 |
| Goal unattended limit | 150 |
| Goal user grace | 10000 ms |
| Goal objective limit | 4000 chars |
| Goal turns before blocked | 3 |
| Goal recent hashes kept | 3 |
| Status objective preview | 32 chars |
| Arbiter quiet after the last job end | 2 s |
| Arbiter waiting-reports trigger | 16 |
| Arbiter oldest-report trigger | 60 s |
| Injection budget | 16384 bytes |
| Status update rate | 2 per second |
| Battery mailbox | 256 |
| Workflow steps | 1..=32 |
| Prompt length | 1..=32000 chars |
| Pool items | 1..=1024, each 1..=2000 chars |
| Pool workers | 1..=64, default 4 |
| Step tools | at most 16 |
| Report length | 1..=16000 |
| child_max_steps | 50, 1..=1000 |
| child_max_minutes | 30, 1..=600 |
| max_runs | 16, 1..=64 |
| Grace turn | 60 s |
| Run notice | at most 12288 bytes |
| Changed paths shown | 3, then `(+<k> more)` |
| `wait` timeout | default 60 s, at most 600 s |
| `list` runs | 20; 200 task lines per run |
| Minimum git | 2.17 |
