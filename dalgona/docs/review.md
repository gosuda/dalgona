# review

The review battery reviews one session's changes with one reviewer
completion per round. It reads only `git status --short` and
`git diff` against the configured base inside the session workspace;
it never writes or runs project code.

Call the `review` tool with an optional `focus`, or run `/review [focus]`
to hand the focus to the model as the next prompt. With no changes the
review reports `No changes to review.` without calling the reviewer.

Each round appends one durable review record. A round converges when the
verdict is clean or no finding is new; otherwise the report lists every
finding marked new or repeat and asks for the new findings. After
`max_rounds` (1 to 10, default 3) non-converged rounds the review session
reaches its cap. At the cap the `review` tool stops, lists the findings
still open, and asks the model to ask you what to do. The model never
starts a new session on its own. You must run `/review` before the model
calls `review` with `restart` set to true; `/review` grants one restart,
and the `/review` command asks the model to call `review` with `restart`
set to true. The `restart` argument only takes effect at the cap; in the
middle of a session it continues the open rounds and never discards them.
The reviewer model is `reviewer_model` (empty selects the session model);
the diff base is `diff_base` (empty selects `HEAD`).
