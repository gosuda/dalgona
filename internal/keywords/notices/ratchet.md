Improve one LLM flow with a ratchet: build a trusted eval, then try one change per round and keep it only when it wins on held-out cases. The `ratchet` tool owns approvals, the frozen split, the completeness checks and the keep or revert decision. Never edit files under `.dalgona/ratchet/` yourself.

Build the eval:
1. Read the code that runs the flow and find its real entry point.
2. Gather 15 to 100 cases with expected answers. Include cases that should trigger the behavior and cases that should not. Note where each expected answer came from.
3. Call `ratchet` with action `init`: case files, harness files (runner and grader), the files you may change, and the files that are off limits.
4. Call action `approve` with stage `inputs`. Put the case ids in the preview, one line each. The preview holds at most 40 lines, so group ids by tag when the set is large. Revise until the user approves.
5. Write a runner that calls the real entry point. It appends one line per case and rep to `results.jsonl` and puts failed attempts in `errors.jsonl`, never a zero score. It writes traces for train cases only and keeps output text out of held-out rows.
6. Choose the cheapest grader that measures the property. A judge model must differ from the model under test.
7. Pilot 3 to 5 cases and read one full row. Then call `approve` with stage `grader` and show five graded pilot cases.

Plan and baseline:
8. Call `split`, then `plan` with the target, guardrails, reps, stop rule and runner command.
9. Call `approve` with stage `plan`. This is the last question for the user.
10. Call `check` for `baseline`, run its command with `exec`, then call `gate` for `baseline`. Read every warning.

Climb, one change per round:
11. Call `train` for the best variant. Give its rows and trace paths to one analyzer, called from an `eval` cell as `tools.task(...)`, because `task` is not a direct tool in `eval-first`. Ask for the behavior that costs the most, quoted evidence and one diff.
12. Apply that one change to the change paths only. Never copy case text into them.
13. Call `check` for the next variant, run the command, then call `gate` with a one-line summary.
14. Follow the decision. On `keep`, build on it. On `revert`, undo the change before the next round; `check` lists files that still differ from the best round. On `rerun`, resume missing slots or add reps to this variant and the best one, then gate again.
15. Report one line per round: decision, test score with its interval, and best so far.
16. When `plateau` is true, sort the remaining train failures by cause before another change. When `done` is true, stop climbing.

Report:
17. Leave the change paths at the best round. Report the test change against the baseline with its interval. Say so plainly when the gain is within noise.

If `approve` declines because no client is attached, stop and say that an interactive session is needed.
