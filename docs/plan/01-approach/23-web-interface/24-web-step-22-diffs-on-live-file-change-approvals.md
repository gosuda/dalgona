#### Web step 22. Diffs on live file-change approvals

- Parser. `parseApprovalRequest` (`validation.ts:782`) stops reading `grantRoot`; changes come from the preceding `item/fileChange/patchUpdated` through `fileChanges` (`validation.ts:244`). The command approval uses `[]`.
- Removed. `approvalChanges` (`+page.svelte:97-102`, now in `Session`).
- Test.
  - `validation.test.ts`: `reads file changes from a live file-change approval`. The fixture is the protocol workstream's payload: Codex members plus extension `changes`.
  - `App.dom.test.ts`: `shows the diff of a live file-change approval before item/completed`.
  - Fails before: the changes come only from the transcript item, which is empty at approval time (`internal/rpc/serve_track.go:336-344`).
- Spec (`docs/spec/web.md`). Step 21 writes the sentence "A file-change approval shows the diffs carried in the request." under `### Approvals`. This step adds no spec text.
- Lands after: Step 21.

Anchor: Richard Feynman.

