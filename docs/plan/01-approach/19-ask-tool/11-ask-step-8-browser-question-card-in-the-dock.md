#### Ask step 8. Browser question card in the dock

Update the existing browser question card and its response lifecycle under the current web and shared input contracts.

Files:

- `web/src/lib/types.ts`:
  - Add `QuestionOption {label: string; description: string; preview: string | null}`, `QuestionItem {id: string; header: string; question: string; options: readonly QuestionOption[]; multiSelect: boolean; isOther: boolean}`, `QuestionRequest {id: RequestId; threadId: string; turnId: string; itemId: string; questions: readonly QuestionItem[]}` and `QuestionSelection {picked: readonly string[]; other: string}`.
  - Add `onQuestion: (request: QuestionRequest) => void` to `RpcHandlers` (`:243-248`).
  - The `requestResolved` notification (`:186`) gains `resolution: string | null`.
  - Widen `ServerRequest` (declared in web Step 23 as `ApprovalRequest`) to `ApprovalRequest | QuestionRequest`.
- `web/src/lib/components/App.svelte`: renders `QuestionCard` when the active request is a question. This step places it in the dock, above the composer, as an inline card beside `ApprovalCard` (Step 21), never as a `<dialog>` (cross-phase decision 26).
- `web/src/lib/question.ts` (new):
  - `export const MAX_ANSWER = 2000;`
  - `parseQuestionRequest(id: RequestId, params: unknown): QuestionRequest | null` checks every vendored required field. An absent `multiSelect` is false, an absent `preview` is null, and `options: null` is a free-text question.
  - `questionResponse` returns the nested vendored answer shape after complete validation, with original labels in option order and accepted Other text unchanged. Invalid text produces a field error and no response. `MAX_ANSWER` counts Unicode scalars, not UTF-16 units or graphemes.
  - `resolutionNotice(resolution: string | null): string | null` uses the step 7 texts.
- `web/src/lib/validation.ts`: the `serverRequest/resolved` case (`:767-776`) reads an optional string `resolution`.
- `web/src/lib/protocol.ts`:
  - `onIncoming` (`:223-267`) handles `item/tool/requestUserInput` before the approval parse. A null parse gives `-32602` `invalid params`; otherwise it calls `handlers.onQuestion`.
  - The private `respondError` becomes public `reject(id: RequestId, code: number, message: string)`; update both callers (`protocol.ts:249` and `:262`).
- `web/src/lib/components/QuestionCard.svelte` (new):
  - An inline card in the dock above the composer, not a modal `<dialog>`. It shows only the oldest pending request (cross-phase decision 26). The request queue belongs to the web workstream, Step 23, and the dock branch is listed above. On arrival it moves focus to `#question-title`, the same as `ApprovalCard` (the web workstream, Step 21).
  - One `<fieldset>` per question with its header as `<legend>`. Use radio buttons or checkboxes and mark option 1 as recommended. Other and free-text fields use scalar-aware validation. Do not enforce the quota with HTML `maxlength`, which counts UTF-16 units.
  - A `<pre>` preview for the checked option, in the code face of the web workstream, Step 9 (`faceMono`, Design record, Type for the web).
  - Actions sit in `<div class="request-actions">`, outside the scrolling body, as `ApprovalCard` does (the web workstream, Step 21): `Submit`, `Skip` (all answers empty) and `Dismiss` (sends the cancel `{answers: {}}`). Esc in the card means Dismiss. The `Submit` button is the card's point (D18): a solid fill in the thread hue (Design record, The point and the thread hue).
  - Text interpolation only, never `{@html}`.
  - Design (Design record, Colour; Design record, The point and the thread hue; Design record, Type for the web; Design record, Components): write the CSS rules of the Design record, Browser cards and Browser controls, with the product's token names. Colours are the Design record, Colour role names only. The card frame is the ruled ledger card (Design record, Components). Its buttons, fields and tags are the soft ledger controls with `min-height: 36px`. Titles, control labels, body text and Hangul text use the type faces of the web workstream, Step 9 (`faceTitle`, `faceText`, `faceMono`). Line heights are unitless. `Recommended` is plain text. Every focusable control draws the Design record, Colour focus ring, 2 px solid `text` with a 2 px offset, and no rule sets `outline: 0` without it. A checked option row takes the `tone` fill and a 2 px `text` bar on its leading edge.
- `web/src/lib/session.svelte.ts` (the `Session` of web Step 4):
  - `answerQuestion(request, selections)` sends `questionResponse(request, selections)` from this step.
  - `cancelQuestion(request)` sends `{"answers": {}}`.
  - Both methods retain the card, draft, and typed response sender until `serverRequest/resolved`.
  - A matching correction warning exposes the field error and permits another response under the same ID and original deadline.
  - Resolution releases the sender. Late warnings cannot restore the card.
  - The complete composition transaction owns its final Enter or Escape event. Rejected commits remain recoverable without a partial insertion.
- `web/src/routes/+page.svelte`:
  - Route `onQuestion` into the shared queue: `trackRequest` (the web workstream, Step 23) takes the `QuestionRequest`. Add no separate `questions` state. Model the handler on `onRequest` (`+page.svelte:151-155`).
  - `requestResolved` (`:544-546`) calls `resolveRequest` (the web workstream, Step 23) and sets the notice from `resolutionNotice`.
  - The status bar shows the word that the web workstream, Step 10 gives the open-question state. This step adds no word of its own.

- Sandbox choice. The sandbox choice is a question with id `sandbox`. Its `warning` notification (`{threadId, message}`) arrives first and shows as a notice. The protocol workstream, Step 13 sends this request. This step queues it and shows the warning. Cancelling it sends `{"answers": {}}`, which counts as no choice (cross-phase decision 29). It joins the queue like any other request. The same card shows it.

Tests in `web/src/lib/question.test.ts`:

- `parses a vendored request`
- `rejects a request without questions`
- `treats null options as free text`
- `orders original labels then unchanged validated other text`
- `accepts 2000 supplementary scalars and rejects 2001 without truncation`
- `returns offered CRLF labels without newline conversion`
- `retains the sender and draft through correction until resolution`
- `preserves committed text when composition commits exceed the limit`
- `maps each resolution to its notice`

Preserve the existing production module and component tests, then add the correction and Unicode cases above.
Component tests must keep actions outside scrolling details and verify dismissal, sandbox cancellation, and the complete keyboard answer path.
Each test must detect its named defect without asserting source text or treating a saved snapshot as behavioral proof.

Docs: [Web questions](../../../dalgona/spec/web.md#questions), [Ask tool](../../../dal/spec/ask.md), and [Input text](../../../dal/spec/input-text.md) define the required behavior.
Automated cases must use the production question card and RPC lifecycle. Real IME checks supplement synthetic browser events.

- Lands after: the web workstream, Steps 4, 8, 10, 21 and 23 (page state, tokens, state words, the approval card and the request queue), and this workstream, Step 3 (the wire table and cancel shape).

Anchor: Edward Tufte, Christopher Alexander.

