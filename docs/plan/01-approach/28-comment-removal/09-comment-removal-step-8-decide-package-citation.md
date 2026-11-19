#### Comment removal step 8. Decide package citation

Anchor: Richard Feynman

- Files: `internal/decide/decisions.go`, `docs/spec/decide.md`.
- Change: delete the comment at `internal/decide/decisions.go:99`. In `docs/spec/decide.md`, in the paragraph that starts `The JSON body MUST contain`, replace `Criteria MUST use the shapes in the [Decisions API schema]` with `Criteria and answers MUST use the shapes in the [Decisions API schema]`. The existing link names the same reference, so no other sentence is added.
- Tests that fail before the change: none. `TestDecisionsAskRejectsInvalidResponses` (`internal/decide/decisions_test.go:46`) stays.
- Check: `just verify` passes.
- Lands after: Comment removal step 7

