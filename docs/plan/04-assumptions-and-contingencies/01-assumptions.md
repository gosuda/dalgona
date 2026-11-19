### Assumptions

- The repository is at commit `3c4b6f94` with a clean tree when the work starts. If it has moved, read each cited file again before you edit it, and apply rule 17 to every line number.
- Approving this plan is the only owner decision that execution needs. Every design choice is in the product decisions, the cross-phase decisions or a step. One matter stays with the owner afterwards: when to turn the edit review on with `[edit_review] enabled = true`, which is off by default.
- The owner chose a structural edit tool on 2026-11-12, dropped the external binary that was to back it, and then confirmed that the plan keeps the tool and builds it on tree-sitter queries (the code-rewrite workstream, phase 19).
- The owner confirmed on 2026-11-12 that both products get the `symbols`, `definition` and `syntax` modes of `search`, and accepted that `dal` grows by the parser and the 23 grammars.
- The owner confirmed the fallback policy at the head of the outside checks on 2026-11-12: the executor takes only a fallback that this plan writes, and a fault that a user can see stops the step.
- The owner confirmed on 2026-11-12 that both products offer the `rewrite` tool. It is called from a script cell in `eval-first` mode, as `patch` is, so it adds no name to the direct list.
- The owner confirmed on 2026-11-12 that one cell makes at most `maxCalls = 1024` tool calls.
- The decision service that the edit review and the approval decider use is reachable only with the owner's credentials. Every test that `just verify` runs uses a stub. One opt-in measurement, Edit-review step 9, calls the live service, and only when `DALGONA_EDIT_REVIEW_EVAL` is set. The live sessions use the scripted model server.
- `uv`, `pnpm 12.18.0` and Node `26.13.0` are installed where the gate runs. Go `1.27.4` is installed after Pins step 1, because CI reads the Go version from `go.mod` (`.github/workflows/ci.yml:33-46`).

