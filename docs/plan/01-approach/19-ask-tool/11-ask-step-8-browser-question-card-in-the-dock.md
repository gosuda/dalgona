#### Ask step 8. Browser question card in the dock

Update the existing browser implementation under [Web questions](../../../dalgona/spec/web.md#questions), [Ask tool](../../../dal/spec/ask.md), [Input text](../../../dal/spec/input-text.md), and the [server-request lifecycle](../../../dal/spec/protocol.md#server-requests).
The files and methods below already exist. Their current ownership determines where this work lands.
These are implementation requirements, not a claim that the current client supports correction.

##### Current ownership

|Source|Current responsibility and required boundary|
|---|---|
|[Route](../../../../web/src/routes/+page.svelte)|Constructs `Session` with an `RpcClient` factory, starts it on mount, stops it on cleanup, and renders `App`. Keep notification handling and request state in `Session`.|
|[RPC client](../../../../web/src/lib/protocol.ts)|`RpcClient.onIncoming` parses questions and calls `handlers.onQuestion`. `respond` writes responses; `reject` is already public. Keep transport parsing and writing here.|
|[Types](../../../../web/src/lib/types.ts) and [validation](../../../../web/src/lib/validation.ts)|Already define question types, the `ServerRequest` union, `RpcHandlers.onQuestion`, and `requestResolved`. Preserve identity and add the correction-warning fields at this boundary.|
|[Reducer](../../../../web/src/lib/reducer.ts)|`trackRequest`, `resolveRequest`, and `activeRequest` manage the immutable ordered queue. They do not own transport effects or notices.|
|[Session](../../../../web/src/lib/session.svelte.ts)|Owns `requests`, `currentRequest`, connection generations, incoming handlers, response methods, settlement, and notices. Own the typed question lifecycle here.|
|[Question helpers](../../../../web/src/lib/question.ts)|`parseQuestionRequest`, `questionResponse`, and `resolutionNotice` parse requests, construct answers, and select notice text. Keep these helpers pure.|
|[App](../../../../web/src/lib/components/App.svelte) and [QuestionCard](../../../../web/src/lib/components/QuestionCard.svelte)|`App` displays `session.currentRequest` and calls Session methods through card callbacks. The card owns DOM interaction, focus, and layout.|

`web/src/lib/requests.ts` contains turn-steering parameters; it is not the pending-request store.
Do not move the Session handlers into `+page.svelte` or introduce a second request queue.

##### Typed notifications and transport

- Keep the existing `QuestionRequest.method` discriminator, `QUESTION_METHOD`, question types, and `RpcHandlers.onQuestion`. They do not need to be added again.
- Extend `AppNotification.requestResolved` and `requestResolvedNotice` to retain and validate the wire `threadId`. The current parser checks for its presence but drops it from the parsed notification. Thread-and-request matching needs both fields.
- Add a typed `warning` notification and parser for the existing message and thread fields plus optional `requestId`, `reason`, and `field`. Correlated correction requires the complete valid identity and error fields. An ordinary warning remains a notice and cannot enable another response.
- Register warnings in Session's `effectKinds` and effects table, preserving the exhaustive notification check. Do not translate correction into a turn failure or a resolved request.
- Keep `RpcClient.onIncoming` as the question parser and dispatcher. Its existing invalid-question path calls `reject` with `-32602`; preserve that behavior.
- Update `RpcPort.respond` and `RpcClient.respond` together so Session can distinguish a locally queued response from a write or closed-socket failure. Today they return `void`, and `send` can report an error then return without sending. A normal return is therefore insufficient evidence for `hadLocalSubmission`.
- Update the existing `answerQuestion`, `cancelQuestion`, and `respondApproval` callers and test ports for that local-write result. Preserve each request kind's settlement policy; a failed local write cannot count as a successful send.
- A successful local queue operation does not acknowledge daemon acceptance. Keep the original JSON-RPC response ID and nested answer shape. Add no winner field or new question method.

##### Session-owned question lifecycle

`Session.openConnection` already supplies `onQuestion` and `onNotification` to the RPC client.
Keep its connection-generation guards and `trackRequest(this.requests, request)` ordering.
Keep `currentRequest` derived through `activeRequest`, including the existing treatment of requests from child threads.

Add typed per-question state owned by Session and keyed by the connection generation, thread ID, and request ID.
It retains the offered request, answer draft, local submission state, pending-send state, correction field error, and ability to send a corrected response.
This state accompanies the existing queue; it is not another queue.
A same-request replay keeps its place and retained draft. It must not reset a submitted request to a fresh editable card.

- `Session.answerQuestion` validates the complete candidate through `questionResponse` before calling `requireClient().respond`.
- `Session.cancelQuestion` sends the existing empty answers map.
- Remove immediate `settleRequest` calls from both question methods. A successfully queued response sets `hadLocalSubmission`, keeps the card and draft, and waits for a warning or resolution.
- A failed validation or local write leaves the draft recoverable with its error and does not set `hadLocalSubmission`. If an earlier response was queued, its existing true value remains true.
- Block duplicate sends while awaiting resolution. A matching correction warning restores editing and permits one corrected response under the original ID. Preserve the original deadline; the client does not start a replacement question or timer.
- Handle correction in Session's warning effect after matching the current connection generation, thread, request, and pending lifecycle. Show the supplied field error through the card. Stale warnings cannot reopen a settled request.
- Preserve ordinary approval handling in `respondApproval` and its patch cleanup. Question correction does not redefine approval outcomes.
- On connection replacement or shutdown, end the old generation's actionable lifecycle with the existing connection behavior. Do not replay an uncertain response or promise that the host still has the question after its last subscriber leaves.

`Session` also owns the `requestResolved` effect. Preserve its existing resolution handling for approvals and other request kinds.
Apply the following order only in the question branch:

1. Find the pending question using both notification `threadId` and `requestId` within the current generation. A stale, duplicate, or unrelated resolution changes no other question or notice.
2. Capture its `hadLocalSubmission` and original request metadata before calling `settleRequest` or releasing lifecycle state.
3. Compute `resolutionNotice(notification.resolution, hadLocalSubmission)` from that captured state. An `answered` or `cancelled` outcome uses a neutral notice after any locally queued response. Only a request with no local submission may use the other-window text from Step 7.
4. Apply the existing timeout marker to the matched question when appropriate. Then call `settleRequest` once and release that question's sender and draft state. `settleRequest` remains the owner of `resolveRequest` and `forgetPatchChanges` cleanup.
5. Publish the computed notice through `Session.notify`. A generic resolution neither identifies the winning client nor promotes local values to accepted answers.

The existing `requestResolved` effect currently calls `settleRequest` before `resolutionNotice`; change that order inside Session.
The route has no `requestResolved` handler and must not receive one for this work.

##### Answer construction and card state

- Replace `answerTexts` trimming and truncation in `question.ts` with complete validation. `questionResponse` returns the nested vendored answer shape only for a valid candidate; invalid text yields a typed field failure and no response.
- `MAX_ANSWER` remains 2000 Unicode scalars. Preserve option order, exact original labels, and accepted typed text. Selected labels containing CRLF bypass the authored-draft newline adapter.
- Apply the shared newline policy once at the editable field boundary. Do not let a DOM field silently remove line breaks or split an invalid edit. Reject malformed encoding and NUL without replacement decoding.
- Extend the existing `resolutionNotice` helper to take `hadLocalSubmission`. Pure helper tests cover both local-history states; the helper must not query or mutate Session.
- Move retained answer values into the Session-owned question lifecycle and pass them through `App` to `QuestionCard` with explicit update callbacks. Keep focus, selection, and composition handling in the component. The current component-local `selections` reset on request updates must not erase a pending or corrected draft.
- `App` continues to render `QuestionCard` for `isQuestionRequest(session.currentRequest)` and route Submit and Dismiss through `session.answerQuestion` and `session.cancelQuestion`. Pass the lifecycle's pending and field-error state through this same path.
- Remove HTML `maxlength` as quota enforcement. Validate complete edits using scalar counts, preserving the previous committed text and the recoverable rejected candidate.
- Keep the complete IME transaction through its final committing Enter or Escape event. The existing `isImeKey` guard alone is not evidence that this transaction works on an actual input method.

##### Existing web design

Keep the inline dock card and shared request ordering.
Show queue position, one fieldset per question, radio or checkbox choices, and the first option marked `Recommended`.
Keep the title focus target, question descriptions, monospace previews, text interpolation, and existing focus handoff to the composer.
Submit remains the sole point-filled action. Skip submits an empty list for every question; Dismiss submits the empty answers map.
Keep the actions outside the scrolling question details and preserve keyboard navigation, visible focus, existing theme tokens, and the web card's spacing.
Pending submission and correction errors must fit this card without creating a modal or an additional request queue.

The sandbox choice uses the same question queue and card.
Its preceding uncorrelated warning remains a notice. It cannot authorize a correction or change local submission state.
Dismissing that question retains its existing no-choice meaning.

##### Behavioral verification

|Production boundary and test file|Required observations|
|---|---|
|[Question helpers](../../../../web/src/lib/question.test.ts)|Keep vendored parsing and null-options coverage. Replace the existing trim-and-cap expectation with exact-value preservation, 2000 versus 2001 scalar validation, supplementary scalars, offered CRLF labels, and both notice-attribution states.|
|[Notification validation](../../../../web/src/lib/validation.test.ts)|Retain `threadId` on parsed resolutions. Preserve warning identity and field-error data, distinguish ordinary warnings, and reject malformed correlation fields.|
|[RPC transport](../../../../web/src/lib/protocol.test.ts)|Exercise actual `RpcClient` parsing and response writes. A closed socket or failed write reports local failure; a queued response preserves its ID and exact answer payload without claiming server acceptance.|
|[Request reducer](../../../../web/src/lib/reducer.test.ts)|Preserve queue order, same-request redelivery, active-thread selection, and removal of only the request Session matched.|
|[Session](../../../../web/src/lib/session.test.ts)|Drive `onQuestion`, `answerQuestion`, correction warnings, and `requestResolved` through the production Session. Capture outgoing fake-port responses instead of leaving `FakePort.respond` as a no-op. Verify retained state after first send, correction under the same ID, state capture before settlement, neutral notices, local-send failure, stale or wrong-thread events, and generation replacement.|
|[Question card](../../../../web/src/lib/components/QuestionCard.dom.test.ts)|Use `App` with a real Session for the submit-warning-correct-resolve sequence. Verify exact draft preservation, one response per allowed submission, pending controls, field errors, keyboard operation, dismissal, sandbox cancellation, actions outside scrolling details, and the composition regressions below.|

Composition regression tests must drive the actual `QuestionCard` DOM through `App` and Session, with captured outgoing responses.
Use the [composition lifecycle](../../../dal/spec/input-text.md#i06-own-the-full-composition-lifecycle) and [cross-client cases](../../../dal/spec/input-text.md#i12-prove-cross-client-behavior) as independent expected behavior.

- Commit CJK preedit with Enter under both event orderings: the final key before `compositionend`, and `compositionend` before the final key reports composition inactive. The complete transaction inserts exactly once and produces no answer response. The commit key must not also trigger Submit, Skip, Dismiss, or a composer action. A separate deliberate Submit after completion sends exactly one response with the committed value.
- Cancel preedit with Escape under both event orderings, including `compositionend` before the final Escape reports `isComposing === false`. Preserve the prior committed draft and selection, emit no response, and keep the same pending question open. The cancellation key cannot dismiss the card or interrupt the thread.
- Navigate IME candidates with Arrow and Tab keys while the input method owns them. Application option navigation, focus shortcuts, history, and palette actions must defer and produce no response.
- Commit a supplementary-scalar or combining-text replacement across the 2000-scalar boundary. An accepted boundary commit remains exact. An over-limit commit leaves the previous committed draft and selection unchanged, retains the complete rejected candidate for correction, and emits no response. It must not insert a valid prefix, truncate a grapheme cluster, or count UTF-16 units as the answer quota.
- Commit an accepted composition over a nonempty selection. One Undo restores the exact previous text and selection; one Redo restores the exact committed replacement. Neither action sends a response, and the commit does not create multiple undo steps.
- Exercise a noncancelable composition/input event and a rejected replacement of an existing selection. Reconcile the DOM with retained Session state while preserving the rejected candidate. A matching correction warning must not erase the draft or reset the pending request identity and deadline.
- Switch the displayed request or move focus during composition. The platform transaction must finish or cancel before the application action continues. It must not transfer preedit, an IME key, or a response into another question.

Session tests must observe the request remaining queued after Submit or Dismiss and disappearing only on its matching resolution or the existing connection-ending path.
The locally submitted case must use a neutral notice after resolution; the no-local-submission case keeps its supported other-window notice.
Test preservation across same-request replay and switching the displayed request, not only an uninterrupted component instance.
Expected values come from the contracts. Source-text assertions or a standalone notice-helper test cannot prove Session ordering.
Real supported browser and IME checks supplement synthetic component events.

Lands with Step 3's typed transport lifecycle and the existing shared request queue and dock.
