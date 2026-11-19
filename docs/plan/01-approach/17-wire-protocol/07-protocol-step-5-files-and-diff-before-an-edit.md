#### Protocol step 5. Files and diff before an edit approval

- `internal/agent/types.go:142-153`: `ApprovalPrompt.Files []ApprovalFile`; `type ApprovalFile struct { Path string; Pre, Post []byte; Added, Deleted, TooLarge bool }`. Content is masked; a side above `textdiff.Limit` sets `TooLarge` and nil bytes. `Added` is true when the change has no pre image (`PreSHA` empty) and `Deleted` when it has no post image (`PostSHA` empty).
- `internal/agent/gate.go:24-34` `gateCall.files []tool.FileChange`, set in `worker.gateCall` (`internal/agent/tools.go:276-279`; `gc.files = plan.Files` right after `p.plan = &plan`). `worker.human` (`gate.go:255-262`) fills `prompt.Files` through `func (w *worker) approvalFiles(files []tool.FileChange) ([]ApprovalFile, error)`; a mask error returns `decline("mode:ask", "declined: secret masking failed")`.
- `internal/rpc/types.go`: `FileChangePatchUpdatedNotification{ThreadID, TurnID, ItemID string; Changes []FileChange}` with a writer that calls `FileChange.write` (Step 4), so a change with `diffOmitted` reaches the approval list.
- `internal/rpc/serve_approval.go` `ask` (line 22): for `tool.ClassEdit`, build one `FileChange` per `prompt.Files` entry with `diffChange(f.Path, kind, f.Pre, f.Post, f.TooLarge)`, where `kind` follows the rule of `fileChanges`, store them on `w.tracker.items[prompt.CallID]` when that item exists and `update.Thread == w.thread.ID()`, send `item/fileChange/patchUpdated` before the approval request is spawned (`spawnRequest`, `serve_approval.go:83`, named `spawnApproval` until the ask workstream, Step 3 renames it).
- Test `internal/rpc` `TestEditApprovalSendsTheDiffFirst`: rig `config.ApprovalAsk` with the patch tool; `c.order` shows `item/fileChange/patchUpdated` before `item/fileChange/requestApproval`, and its `changes[0]` has `additions == 1`. Fails before: no such notification.
- Docs: `docs/spec/protocol.md`, `## Server requests`: one paragraph for `item/fileChange/patchUpdated`. Step 13 adds the sandbox paragraph there, and the ask workstream, Step 3 adds the question paragraph.
- Lands after: Step 4, the edit-review workstream, Step 1, the ask workstream, Step 3 and Step 3a.

Anchor: Dieter Rams, Tony Hoare.

