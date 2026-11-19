#### Terminal step 5. Follow-ups are never cleared silently

- `apply.go` `setThread` (`model.go:552-566`): delete `m.followups = nil` (`model.go:557`). The queue is `m.queue` after step 1.
- `commands.go` `applyNewThread` (`commands.go:578`), `applyResumed` (`commands.go:557`) and `applyForked` (`commands.go:604`): when `len(m.queue) > 0`, call `noticef("%d follow-ups moved to %s; press Enter on an empty prompt to send the next", n, title)`. The notice is one row in the live row (step 2). Queued items start only through `startQueuedFollowup` (`commands.go:170-180`), as today.
- The queue is state. A thread switch prints no row for it and removes nothing from the transcript.
- Test (fails before): replace `TestSetThreadClearsFollowups` (`setthread_test.go:11`) with `TestThreadSwitchKeepsFollowups`. Input: queue of two items (`a`, `b`), then the `new` result is applied. Expected: `len(m.queue) == 2`, and the notice contains `2 follow-ups moved to`.
- Test (fails before, file `dock_test.go`, `TestFollowupNoticeKeepsDockHeight`): the dock height before and after the notice is equal. Source: design I2.
- Spec: `docs/spec/tui.md` keys section, the follow-up paragraph.
- Lands after: Steps 1, 2 and 3.
- Anchor: Milton Erickson.

