# Review

The review extension reads `git status` and `git diff`, uses shared run and inference services, and appends one durable review record before returning. The `/review` command and `review` tool do not create a child reviewer or call the judge question API.
