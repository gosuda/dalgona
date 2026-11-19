#### Comment removal step 2. Remote package comments

Anchor: David Parnas

- Files: `internal/remote/cloud.go`, `internal/remote/lifecycle.go`, `internal/remote/main.go`, `internal/remote/pull.go`, `internal/remote/remote.go`, `docs/spec/runners.md`.
- Change: delete the comment lines of rows 1 to 22. Rows 6, 12, 13, 16 and 21 also add the spec text below.
- Spec text, `docs/spec/runners.md`: row 6 replaces, in `## Archive`, the sentence `(*Binder).Archive(ctx, threadID)` MUST run before `thread/archive` journals `thread.archived`. with the two sentences `(*Binder).Archive` MUST run as the archive hook, after `Host.Archive` journals `thread.archived`. and `If the archive hook fails, the server MUST unarchive the thread.` Row 12, as a new paragraph before the paragraph that starts `Pull MUST NOT move the baseline.`: `Files that `git apply --3way` leaves with conflict markers MUST be listed in `ConflictError`.` Row 13, in the same place, after the row 12 paragraph: `In a git work tree, `Binder.Pull` MUST return a `*ConflictError` that lists the unmerged files.` Row 16, as a new paragraph after the paragraph that starts `The `Main` functions MUST return 0`: `Without `Options.Journal`, `Pull`, `Archive`, `GC`, and runner binding MUST fail.` and `` `CloudUp`, `CloudDown`, and `CloudPull` MUST NOT need `Options.Journal`. `` Row 21, as a new paragraph after the paragraph that ends `With no recorder, binding MUST fail.`: `The `Attach` hook of the daemon host MUST call `SetRecorder` with the new `*agent.Host`, because the host is the recorder.`
- Tests that fail before the change: none. Behaviour does not change. `TestGCDestroysConfirmedUnboundBoxes` (`internal/remote/remote_test.go:267`) and `TestApplyDiffReportsConflicts` (`internal/remote/pull_test.go:13`) stay green.
- Check: `just verify` passes. `grep -n "MUST run before" docs/spec/runners.md` returns no line.
- Lands after: Comment removal step 1

