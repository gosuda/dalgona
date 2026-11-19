#### Comment removal step 18. Runner package comments

Anchor: Tony Hoare

- Files: `internal/runner/bootstrap.go`, `internal/runner/wire.go`, `docs/spec/runners.md`.
- Change: delete the comment lines of rows 70 to 72 (`bootstrap.go:40-42`, `bootstrap.go:628-629` and `wire.go:35-36`).
- Spec text, `docs/spec/runners.md`: row 70, as a new paragraph after the paragraph that starts `` `Close` MUST close the exec stream `` under `## Mode A: thread boxes`: `Wait` on an exec stream MUST return after `Close`, so the worker goroutine can exit. Row 72 needs no sentence: `docs/spec/sandbox.md` under `## Remote` already states the refusal.
- Tests that fail before the change: none for the spec rows. Row 71 names the `Redialer` interface (`internal/runner/bootstrap.go:630-632`), pinned by `TestStopProcessWithGrace_signalsOverFreshConnectionAfterStreamLoss` (`internal/runner/bootstrap_redial_test.go:56`).
- Check: `just verify` passes.
- Lands after: Comment removal step 17

