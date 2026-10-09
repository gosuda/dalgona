# Orchestration

The orchestration extension coordinates background jobs, monitored output, goals, and child-agent workflows. Use `agents` to run, wait for, cancel, or list workflows. Use `monitor` to watch matching output from a background job. Use `create_goal`, `update_goal`, and `get_goal` with `/goal` to manage a durable session goal. `/continuation` controls automatic turns, and `/abort` cancels active orchestration work. Automatic reminders are delivered only when the session is ready; child reports are claims, not proof.

## Jobs and cancellation

Jobs run under one owner task per session with a bounded queue. `cancel` names a job or a turn and ends it through one cancellation tree; cascade cancellation reaches every descendant. Host shutdown closes every open session and stops timers, child sessions, processes, and background jobs before the process exits.

## Agents, scopes, and budgets

Child agents are admitted in FIFO order into a scope. A scope carries a budget of requests, tokens, and usd, and usage rolls up through nested scopes to the parent ledger. Admission fails when the budget is exhausted or a usd-budgeted model has no known price. A scope opened inside a hook dies at that hook's deadline. Reports from children are claims, not proof: rebuild the promised scope, inspect the changed files, and run the checks before trusting one.

## Wake and turns

A turn started by a wake rather than a user prompt counts against a limit of 20 consecutive wake-started turns. The twenty-first wake is refused and the refusal is journaled; a user prompt resets the count.

## Mailbox

Agent-to-agent mail travels through the journal-backed mailbox with per-pair FIFO order and cursor-based reads. The delivery mode is `aside`, `steer`, or `next_turn`: an aside is delivered without steering the current turn, steer joins the running turn, and next_turn queues for the following one. A send returns a receipt naming the outcome: delivered, woken, buffered, full, or gone.

## Synthetic models and private tools

A synthetic model is a model route whose handler emits a normal event stream; usage inside it is journaled against the run that made it. A handler may bind private tools that exist only for that call; a private tool that is not declared in the request may not shadow a session tool, and the collision fails closed.
