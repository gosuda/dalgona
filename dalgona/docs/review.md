# review

The review battery reviews one session's changes with one reviewer
completion per round. It reads only `git status --short` and
`git diff` against the configured base inside the session workspace;
it never writes or runs project code.

Call the `review` tool with an optional `focus`, or run `/review [focus]`
to hand the focus to the model as the next prompt. The workspace must be
a Git repository. With no changes, review reports `No changes to review.`
without calling the reviewer.

`git status --short` must fit within 262,144 bytes. Review refuses larger
status output and asks you to commit or stash unrelated changes; it does
not truncate status. A diff larger than 262,144 bytes is truncated, and
review proceeds with the prefix and a truncation notice. More than 50
reviewer findings fail the round; resolve the reported findings first.

Each round appends one durable review record. A round converges when the
verdict is clean or no finding is new; otherwise the report lists every
finding marked new or repeat and asks for the new findings. After
`max_rounds` (1 to 10, default 3) non-converged rounds the review session
reaches its cap and stops. The review then reports the findings still open
and asks the user what to do. The model never starts a new session on
its own. The user must run `/review` before the model calls `review`
with `restart` set to true; `/review` grants one restart. The
`/review` command asks the model to call `review` with `restart` set to
true.
`restart` only takes effect at the cap; mid-session it continues the open
rounds. The next review call consumes the restart grant regardless of its
arguments. The reviewer model is `reviewer_model` (empty selects the
session model); the diff base is `diff_base` (empty selects `HEAD`).

Each round probes the reviewer route's credentials, then sends one
request under a 120 second deadline. A route without usable credentials,
a provider error, and a missed deadline each fail the round with
`reviewer call failed: <cause>`; none appends a record. Cancelling the
call appends no record and leaves the session open. When the provider
reports token usage, the round also posts one `review` notice with the
input and output counts.

Cut text ends with a marker. A stored finding `detail` is cut to 500
bytes and an error cause to 200 bytes, each ending with ` [truncated]`.
A `focus` over 500 bytes is cut to 500 bytes and ends with
`[... focus truncated]`.
