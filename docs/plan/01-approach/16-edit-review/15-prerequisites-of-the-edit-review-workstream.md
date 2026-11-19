#### Prerequisites of the edit-review workstream

- The protocol workstream, Step 1 MUST land before Step 6. It creates `internal/rpc/testdata/dalgona-schema/` and `internal/rpc/schema_dalgona_test.go`, and Step 6 adds two files and two names to them.
- The protocol workstream, Step 5 (`Files and diff before an edit approval`) adds the masked post-image blob to each file change, and it MUST land before Step 4a. Until then the turn review cannot read post-images, and it lists those paths in `skipped`.
- Step 1 of this workstream MUST land before the protocol workstream, Step 4 (`Unified diffs with line counts`), the first protocol step that calls `internal/textdiff`. Any protocol workstream step that edits the `agent.NewHost(agent.HostConfig{...})` block at `internal/app/app.go:88-94` merges that edit into the changes of Steps 3 and 4 of this workstream (line 91).
- The terminal workstream, Steps 9, 14 and 16 MUST land before Step 7. Step 7 uses the verbosity setting on `screenState`, the theme roles `roleWarningText`, `roleFailedText`, and `roleDim`, and the `commit` print path in `rows.go`. It also uses `m.screen.wrap` (the terminal workstream, Step 2) and `sanitizeText` in `internal/tui/render.go` (line 52 at commit `3c4b6f94`).
- The web workstream, Steps 8, 16 and 17 MUST land before Step 8. Step 8 uses the colour roles of Step 8, adds its review line to `rows/FileChangeRow.svelte` from Step 17, and places the turn line after the rows that Step 16 creates.
- The manual run in the section "Checks of the edit-review workstream" assumes the scripted chat-completions server from the live-debugging steps (the live-debug workstream, Step 1 and 2, and product decision 16).
- A terminal workstream that needs line diffs should import `internal/textdiff` rather than add a second diff.
- The protocol workstream, Step 4 defines `diffChange` in `internal/rpc/diff.go`, which calls `textdiff.Unified`. Step 1 of this workstream sets the `context` parameter, so `diffChange` passes 3 and its diffs do not change. The protocol workstream, Step 5 calls `diffChange` and needs no change.
- The `ratchet` tool from product decision 10 is separate; Step 9 does not depend on it.
- Order inside this workstream. Step 1 lands before Step 4. Step 3 lands in phase 7, before the protocol workstream, Step 8. Phase 10 runs Steps 2, 4, 5, 4a, 6, and 9 in that order. Step 7 lands in phase 14, after the terminal workstream, Steps 9, 14 and 16. Step 8 lands in phase 15, after the web workstream, Steps 8, 16 and 17.

