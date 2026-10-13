# Goal

A goal is one durable objective for the session. The model manages it through `create_goal`, `update_goal`, and `get_goal`; you manage it through `/goal`.

## Commands

`/goal <objective>` creates or replaces the goal. `/goal` shows it. `/goal pause` pauses continuations, `/goal resume` resumes them, and `/goal clear` removes the goal. `create_goal` fails while an unfinished goal exists; `update_goal` moves the status to `paused`, `blocked`, or `complete`.

## Objective

The objective is 1 to 4,000 characters. A longer one is refused, never truncated: put the full objective in a file and name the file in the objective.

## Continuations

While a goal is active, the battery continues it automatically: right after an automatic turn, ten seconds after a turn you started, and at idle wakes. Continuations stop on their own when the work ends, and every continuation counts against guards you can raise with `/goal pause` or `/continuation pause`:

- 8 consecutive continuations on one progress signature block with `continuation cap reached`;
- 150 continuations without a message from you block with `unattended continuation limit reached`;
- three identical outputs in a row block with `repeated assistant output`;
- a second output-truncation stop after one recovery blocks with `output truncation repeated`;
- a turn that ended on a provider error blocks with `provider error ended the turn (retries exhausted)`.

A message from you resets the consecutive count, and a paused or blocked goal continues when you speak again. `/continuation run` starts automatic turns; `/continuation pause` and `/continuation stop` stop them. The last mode survives the session in the goal sidecar.

## Storage

A goal lives in `goal.json` beside the session and only for a saved session: an ephemeral session refuses goals. Reopening the session restores the goal, its counters, and the last controller mode. A damaged goal file arms nothing; only `/goal clear` replaces it.
