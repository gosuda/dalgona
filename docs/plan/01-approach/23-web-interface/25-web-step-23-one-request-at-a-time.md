#### Web step 23. One request at a time

- Owner. This step owns the request queue for every kind of server request. It declares no question type, parser, response, notice text, handler or test. The ask workstream, Step 8 declares the question kind.
- Types. `type ServerRequest = ApprovalRequest` in `types.ts`. The ask workstream, Step 8 widens it to include `QuestionRequest`.
- Queue (cross-phase decision 26). Approvals, questions and the sandbox choice share one queue in arrival order. `trackRequest`, `resolveRequest`, `activeRequest` and `requestsElsewhere` replace `trackApprovalRequest`, `resolveApprovalRequest`, `activeApproval` and `otherThreadApprovalCount`, and take `ServerRequest`. `activeRequest` returns the oldest pending request of the open thread. Update every caller and test. `itemsAwaitingApproval` filters approvals.
- Placement. The dock renders the active request above the composer, as an inline card beside `ApprovalCard` (Step 21), never as a `<dialog>` (cross-phase decision 26). The ask workstream, Step 8 adds the question branch to that slot.
- Tests.
  - `web/src/lib/components/App.dom.test.ts`, describe `request queue`:
    - `shows the oldest request and holds the next until it resolves`
    - `a sandbox choice waits behind an approval`
  - Fails before: `activeApproval` knows approvals only, and no queue holds a second request. The test `a sandbox choice waits behind an approval` uses two requests of the kinds that exist when this step lands; the ask workstream, Step 8 adds the question case to it.
- Spec (`docs/spec/web.md`). Step 21 holds the sentence on one request at a time, so this step adds no spec text. The ask workstream, Step 8 writes `### Questions`.
- Lands after: Steps 4, 5 and 21.

Anchor: Milton Erickson.

