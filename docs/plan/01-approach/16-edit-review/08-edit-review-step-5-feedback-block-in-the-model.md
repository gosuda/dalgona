#### Edit-review step 5. Feedback block in the model request
Anchor: Milton Erickson, Claude Shannon

Files and symbols:
- `internal/agent/context.go`:
  - In `transcript.add` (line 115), add `case event.ToolJudged: b.judged(value)` and `case event.TurnJudged: b.turnJudged(value)`.
  - `func (b *transcript) judged(e event.ToolJudged) { if e.Feedback { b.emitUser(reviewFeedback(e)) } }`.
  - `func (b *transcript) turnJudged(e event.TurnJudged) { if e.Feedback { b.emitUser(reviewTurnFeedback(e)) } }`.
  - `emitUser` holds the block back while results are outstanding (lines 171-174), so an edit block always follows its round's results. A turn review runs with no outstanding results, so its block is emitted at once.
- `internal/agent/review.go`:
  - `func reviewFeedback(e event.ToolJudged) string` and `func reviewTurnFeedback(e event.TurnJudged) string`. Both build only from journal values: the `spoken` ids, the call id or turn id, and `paths`. `html.EscapeString` applies to every attribute value. `paths` are joined with `, `. Each spoken id maps to its `feedback` sentence in `reviewQuestions` (Step 4). An id that is not in the table renders as `The review raised the flag <id>.`
  - `reviewQuestion.feedback` is defined in Step 4. This step adds no field.
- Templates. An edit block:
```text
<edit-review call="call_7" paths="internal/model/retry.go">
An automatic review of this edit found possible problems:
- <the feedback sentence of each spoken id, in journal order>
Check the changed lines. If a problem is real, fix it. If not, ignore this review.
</edit-review>
```
A turn block:
```text
<turn-review turn="turn-3">
An automatic review of this turn found possible problems:
- <the feedback sentence of each spoken id, in journal order>
Check the changed files. If a problem is real, fix it. If not, ignore this review.
</turn-review>
```
- Cache. The block is the last message of the request, after the round's results. Every earlier message keeps the same bytes in the next request, so the provider's prompt-cache prefix still matches. A clean verdict adds nothing, so the request is the same as it is with review off. The Anthropic encoder puts the cache marker on a user text block when the message has no tool results (`internal/model/anthropic/anthropic.go:200-210`), and this block is such a message.
- Rebuilds read the journal. A rebuilt request reads `spoken` from the event and never the current speaker set, so an old journal renders the same bytes.

Failing-first tests (`internal/agent`):
- `TestEditReviewFeedbackFollowsToolResults`: after a flagged round, the request has the edit's `function_call_output` followed by one user text equal to the edit block built from its spoken sentence, with `call="call_7"` and the path.
- `TestEditReviewCleanVerdictLeavesRequestUnchanged`: the last input item is the `function_call_output`, and no text contains `<edit-review`.
- `TestEditReviewFeedbackKeepsRequestPrefix`: every input item of request k equals the matching item of request k+1, compared with `reflect.DeepEqual` on `inputItems`.
- `TestTurnReviewFeedbackIsLastUserMessage`: after a speaking turn review, the request ends with a user text equal to the turn block with `turn="<turn id>"`.
- `TestFeedbackBuiltFromJournaledSpoken`: the journal holds a `tool.judged` with `spoken` = `["lost_code"]`, and the host's `EditFeedback` is now `["placeholder"]`. Expected: the rebuilt request contains the `lost_code` sentence and does not contain the `placeholder` sentence.
- `TestReviewFeedbackEscapesAttributes`: the call id is `call"7<x>` and the path is `a&b.go`. Expected: the attributes read `call="call&#34;7&lt;x&gt;"` and `paths="a&amp;b.go"`, the output of `html.EscapeString`.

Spec:
- `docs/spec/events.md`, under `## Model context`, add one bullet, which this step owns: "A `tool.judged` with `feedback` true MUST render as one user message at its journal position, after the round's tool results, using the edit-review template. A `turn.judged` with `feedback` true MUST render as one user message at its journal position, using the turn-review template. An event without `feedback` MUST NOT render. Both templates use only journaled `spoken` ids and fields."
- `docs/spec/edit-review.md`: a Feedback section with both templates and the cache rule.
- Lands after: Steps 2 and 4.

