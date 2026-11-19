#### Comment removal step 10. Tool package comment

Anchor: Italo Calvino

- Files: `internal/tool/tool.go`, `docs/spec/edit-styles.md`.
- Change: delete the three comment lines of `Plan.Discard` (row 53, `internal/tool/tool.go:87-89`).
- Spec text, `docs/spec/edit-styles.md`: as a new paragraph after the paragraph in `## Planning and recovery` that contains `a second release MUST succeed.`: A plan that will not be applied, such as a declined approval, MUST be released through `Discard`.
- Tests that fail before the change: none. Evidence: `TestDiscardPlanIsIdempotent` (`internal/agent/plan_discard_test.go:125`) and `TestDeclinedPlanIsDiscarded` (`internal/agent/plan_discard_test.go:39`).
- Check: `just verify` passes.
- Lands after: Comment removal step 9

