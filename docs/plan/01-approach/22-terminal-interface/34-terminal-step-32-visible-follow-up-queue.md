#### Terminal step 32. Visible follow-up queue

- `dock.go`: the queue is a fixed field, not a region. Two places show the count: the live row (`<n> queued`, Step 11) and the status band (the `queue` segment, Step 12). Both have a fixed width, so a change of the count moves no other field. `alt+enter` in the editor calls `queueFollowup` (`internal/tui/model.go:200-201`), which adds the draft to the queue. This step owns that key, and the web workstream cites it.
- Queued texts show in a panel. `alt+q` opens it. It is a `panelList` (cross-phase decision 53), and its item rows are `panelRows(panelList, height) - 2`. Each row reads `<i>. <first line>`. The list cursor scrolls the rows, so every queued text is reachable. `up` and `down` move the cursor, `enter` does nothing, and `esc` closes. Opening and closing the panel are the key events that may change the dock height (cross-phase decision 52).
- After an interrupted or failed turn with follow-ups held, the live row (idle) shows the notice `Held · <n> · Enter on an empty prompt sends the next`. The live row carries it (Step 11). The earlier `Queued · n` header is deleted.
- `alt+up` on an empty editor moves the newest queued item back into the editor. `shift+up` is not bound.
- Symbols that change: the queue region above the editor is deleted. `alt+q` is a new key, and the queue panel is a `panelList` (Step 21).
- Tests (fail before). `TestQueueVisible` is replaced by two tests.
  - `TestQueueCountIsFixedField`: with one queued item and then two, the live row's free text starts at the same column, and the band shows the count 2. Source: the section "Terminal screen design", fixed fields.
  - `TestQueuePanelListsTexts`: with two queued texts, `alt+q` shows the rows `1. <first line>` and `2. <first line>`. The panel is 12 rows at a window of 24 rows. Source: cross-phase decision 53 and this step.
  - `TestAltUpDequeues` (kept).
- Spec: adds two rows to the keys table of `docs/spec/tui.md`: `alt+q` opens the queue panel, and `alt+up` moves the newest queued text back into the editor. This step adds no other spec text.
- Reference: grok-build `app/queue_edit.rs:1-60` and the omp2 `Dequeue` action (via the feature reports). Taken: a visible queue and pull-back. Left: in-place queue editing.
- Lands after: Steps 3, 11, 12, 13, 19 and 21.
- Anchor: Otl Aicher

