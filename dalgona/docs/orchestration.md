# Orchestration

The orchestration extension owns session-scoped goal, monitor, wake-arbiter, agent, worktree, loop-guard, and sleep behavior. It uses dal's `agents`, `jobs`, `turn`, `sidecar`, `run`, and `ask` services. Automatic wakes are bounded by the core limit of 20 consecutive wake-started turns without a user prompt. Job and child reports remain claims until their promised scope and changed files have been checked.
