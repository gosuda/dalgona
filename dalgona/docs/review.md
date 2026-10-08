# Review

The review extension reads `git status` and `git diff`, uses shared run and inference services, and appends one durable review record before returning. The `/review` command and `review` tool do not create a child reviewer or call the judge question API.

A review session ends when a round converges or `max_rounds` rounds pass with new findings. At the cap the `review` tool stops, lists the findings still open, and asks the model to ask you what to do. After the cap the model never starts a new session on its own: `restart` set to true restarts only when the same session ran `/review`, and one `/review` authorizes one restart. Run `/review` after the cap to start a new session: the command asks the model to call `review` with `restart` set to true. The `restart` argument only takes effect at the cap; in the middle of a session it continues the open rounds and never discards them.
