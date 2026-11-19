#### Web step 21. Approval card with actions always in view

- Removed. `ApprovalDialog.svelte`.
- New `web/src/lib/components/ApprovalCard.svelte`, props `{ request: ApprovalRequest; changes: readonly FileChange[]; position: number; total: number; onDecision }`. `changes` replaces the `changes` prop that `ApprovalDialog.svelte:8` takes today, and Step 22 fills it..
  - Root: `<section class="request-card" aria-labelledby="request-title">`.
  - Header: `<p class="kicker">Approval required{total > 1 ? ` · ${position} of ${total}` : ""}</p>` and `<h2 id="request-title" tabindex="-1">`. The title is `Run this command?` or `Apply these changes?`.
  - Body: `<div class="request-body" role="region" aria-label="Request details" tabindex="0">` with `max-height:min(60dvh,40rem);overflow-y:auto`. It holds the command `<pre>`, the reason, `DiffCard`s (open) and `From subagent <title>` for child requests.
  - Actions: `<div class="request-actions">`, outside the body. Buttons: `Accept` (primary, key `y`), `Accept for session` (`a`), `Decline` (`n`), `Cancel and interrupt` (Esc). Esc answers this card at once. The two-press guard of Step 26 applies only to the composer.
  - Keys are handled on the section's `keydown` when the target is not editable.
- Dock. `App.svelte` renders `<div class="dock">` containing `NoticeStack`, then the shown request card, then `Composer`. The shown request is the oldest pending request of the open thread (cross-phase decision 26): `ApprovalCard` for an approval. This step shows that card only; the ask workstream, Step 8 adds the question card to the same slot.
- Focus. On arrival, focus `#request-title`. On resolution, `handoffFocus` targets `#prompt`.
- A `serverRequest/resolved` removes that request from the queue. If it was the shown card, the next oldest request of the open thread shows (cross-phase decision 26).
- Test. `ApprovalCard.dom.test.ts`:
  - `keeps the actions outside the scrolling details`: the region named `Request details` does not contain the `Accept` button.
  - `y accepts and Escape cancels and interrupts`.
  - `ApprovalCard.dom.test.ts`: `shows a nested exec approval with its command and no eval name`. Input: an `ApprovalRequest` with `method` `item/commandExecution/requestApproval`, `itemId` `run-1/2`, `detail` `ls`, `reason` `null`. Expected: the heading `Run this command?`, the text `ls` in the request body, and no text `eval` in the card. Source: the title rule of this step, and `internal/rpc/serve_approval.go:35-44`, where `ItemID` is the prompt's call id. Fails before: no card exists yet.
  - `ApprovalCard.dom.test.ts`: `shows a rewrite change as a diff in a file-change approval`. Input: an `ApprovalRequest` with `method` `item/fileChange/requestApproval`, `itemId` `fc-2`, and `changes` holding one change with `path` `b.txt`, `kind` `update`, `additions` 1, `deletions` 1, `diffOmitted` false, and the same diff text as the Step 17 `rows.dom.test.ts` rewrite test. Expected: the heading `Apply these changes?`, and one open `DiffCard` whose summary reads `Changed`, `b.txt` and `+1 −1`, with one `<del>` of `-beta` and one `<ins>` of `+gamma`. Source: the derivation of the Step 17 test. Fails before: the card takes no `changes` prop.
  - Browser check `web/checks/approval-actions.js` at 320x568 with three long diffs: the Accept button's rect lies inside the viewport while the details are scrolled to the end.
  - Fails before: the actions are the last child of the scrolled content (`ApprovalDialog.svelte:68-72`), and Cancel's label hides the interrupt.
- Nested calls and verbosity. An approval for a call nested in `eval` has `itemId` `<run>/<n>`, the nested call's id (`internal/rpc/serve_approval.go:35-44`). Only `exec`, `edit` and `remote` calls can reach a person. `exec` and `remote` do when no rule or hook decides (`internal/agent/gate.go:132-133`). `edit` does unless the mode is `auto` and no changed path is outside the roots (`gate.go:130-131`). `read`, `agent` and `script` never do (`gate.go:128-129`), so `eval` never asks. The card is the same at every verbosity level, and it does not name the `eval` call, because the request carries no parent.
- Spec (`docs/spec/web.md`). Rewrite `### Approvals`: card, labels, keys, focus, and "Escape cancels and interrupts the turn". The rewrite keeps the sentences that Steps 7, 22 and 23 add: "Requests from other threads appear as one notice inside the workspace with an Open thread action.", "A file-change approval shows the diffs carried in the request.", and "The dock shows one request at a time, the oldest of the open thread. Approvals, questions and the sandbox choice share that order."
- Lands after: Steps 4, 7 and 17 (`Session`, `NoticeStack` and `DiffCard`).

Anchor: Saul Bass.

