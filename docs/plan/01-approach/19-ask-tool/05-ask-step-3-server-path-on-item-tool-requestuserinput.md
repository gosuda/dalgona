#### Ask step 3. Server path on `item/tool/requestUserInput`

Files:

- `internal/rpc/types.go`: add `ToolRequestUserInputParams`, `ToolRequestUserInputQuestion`, `ToolRequestUserInputOption`, `ToolRequestUserInputResponse` and `ToolRequestUserInputAnswer` (the section "Schemas of the ask workstream"). Add `Resolution string `+"`json:\"resolution,omitzero\"`"+` to `ServerRequestResolvedNotification` (`:281-284`).

- `internal/rpc/serve_notify.go`:
  - Add the `write` methods for the three param types. Write `"options":null` when a question has no options, using `o.null()` (`serve_wire.go:56`).
  - `ServerRequestResolvedNotification.write` (`:148-153`) adds `o.optStr("resolution", n.Resolution)`.
- `internal/rpc/serve_approval.go`: rename `spawnApproval` to `spawnRequest` (`:83-95`) and update its caller in `ask` (`:48`).
- `internal/rpc/serve.go`: add the case `agent.UpdateQuestion: s.question(ctx, w, update)` in `drain` (`:322-337`).
- `internal/rpc/serve_thread.go`: in `threadStatus` (`:123-138`), `case event.StatusTurnRunning` (`:129-130`) returns `ActiveFlags: []string{"waitingOnUserInput"}` when `status.Questions > 0` and `[]string{}` otherwise. `status` is the `agent.Status` from `thread.Status()`.
- `internal/rpc/serve_input.go` (new):
  - `const methodRequestUserInput = "item/tool/requestUserInput"`.
  - `type questionAsk struct{ thread *agent.Thread; requestID string; questions []asktool.Question; params ToolRequestUserInputParams }`.
  - `func (s *server) question(ctx context.Context, w *watch, update agent.Update)`:
    - A resolved prompt calls `s.questionResolved`.
    - Otherwise:
      - Call `w.tracker.setStatus("active", "waitingOnUserInput")` (`serve_track.go:102`).
      - Set `prompt := update.Question`, build `params` with `userInputParams(update.Thread, w.tracker.currentTurn(), prompt)`, and set `q := questionAsk{thread: w.thread, requestID: prompt.RequestID, questions: prompt.Questions, params: params}`.
      - Start the request with `askCtx, cancel := context.WithCancel(ctx)` and `started, dup := s.spawnRequest(prompt.RequestID, cancel, func() { defer s.settle(prompt.RequestID); s.requestInput(ctx, askCtx, q) })`. When `dup` is true or `started` is false, call `cancel()` and stop. A question the server cannot start stays open until the thread's last window leaves (D4) or the thread closes. `s.pending` belongs to one connection, because `ServeWith` builds one `server` for each connection (`internal/rpc/serve.go:96`). So `dup` means that this connection already holds the request. Another window gets the question through its own `server`, which is how a window that subscribes late receives it (D9).
  - `func (s *server) questionResolved(w *watch, threadID string, prompt agent.QuestionPrompt)`:
    - Call `s.settle(prompt.RequestID)`.
    - If `w.tracker.turn != ""`, call `w.tracker.setStatus("active")`.
    - Send `serverRequest/resolved` with `Resolution: resolutionName(prompt.Resolution)`.
  - `func (s *server) requestInput(serveCtx, askCtx context.Context, q questionAsk)`:
    - Encode the params; on failure answer `OutcomeFailed` with Detail `the question could not be encoded: <err>`.
    - Call `s.conn.CallID(askCtx, q.requestID, methodRequestUserInput, params, &response)` (`conn.go:112`) with `var response jsontext.Value`, then:
      - no error: `s.answerQuestion(q, inputReply(q.questions, response))`;
      - `askCtx` done, `serveCtx` done, or `io.ErrClosedPipe`: do nothing (the actor handles the last window, D4);
      - an empty answers map is a cancel (see `inputReply`);
      - any other error: `s.declineQuestionIfAlone(q)`.

  - `func (s *server) declineQuestionIfAlone(q questionAsk)` answers `OutcomeNoClient` through `q.thread.AnswerQuestion` only when `q.thread.Status()` reports `Subscribers <= 1`. A status error does nothing.
  - `func inputReply(questions []asktool.Question, response jsontext.Value) asktool.Reply` calls `answersByID(response)`, which returns `(map[string][]string, error)`. An empty answers map gives `OutcomeCancelled` (cross-phase decision 9). This check runs before `asktool.Answers`, so a cancel never reaches the answered row of `Format`. A map that names a question with an empty list is an answer with no choice: `OutcomeAnswered` with every list empty, which `Format` prints as `TextSkipped`. On error it returns `OutcomeFailed` with Detail `the client sent an unreadable answer: <err>`; otherwise `OutcomeAnswered` with `asktool.Answers(...)`. `answersByID` uses `decodeParams`, `readMembers`, `readArray` and `readString` (`serve_wire.go:105-177`), with a helper `readAnswerList`. A missing `answers` member gives the error `answers is missing`.
  - `func (s *server) answerQuestion(q questionAsk, reply asktool.Reply)` calls `q.thread.AnswerQuestion` and ignores `agent.ErrUnknownRequest`, which is the losing answer.
  - `func resolutionName(o asktool.Outcome) string` maps Answered to `answered`, Cancelled to `cancelled`, Interrupted to `interrupted`, and anything else to `declined`.
  - `func userInputParams(threadID, turnID string, prompt agent.QuestionPrompt) ToolRequestUserInputParams` sets `ItemID` to `prompt.CallID` and maps question `i` to `id: asktool.QuestionID(i)`, `isOther: len(options) > 0`, `isSecret: false` and `multiSelect`, plus `isBlocking: true`.

Tests:

- `internal/rpc/schema_test.go`: add `TestCodexSchemasValidateUserInput`. It encodes a params value with `encode(...)` and validates it against `ToolRequestUserInputParams.json`, and validates a response value against `ToolRequestUserInputResponse.json` (with `validateCodexSchema`, `:41`).
- `internal/rpc/serve_input_test.go`, using `newRig(t, config.ApprovalAuto)` (`serve_harness_test.go:138`). The thread comes from `r.host.Resume`; the test calls `thread.Ask` in a goroutine. Every request line is checked against `ServerRequest.json` by `wireClient.route` (`:257-261`).
  - `TestServeQuestionRoundTrip`: the answer becomes `Reply` and `thread/status/changed` carries `waitingOnUserInput` before the request.
  - `TestServeQuestionFirstWindowWins`: two `connect()` clients and `thread/resume`; the second gets `serverRequest/resolved` with `resolution` `answered`.
  - `TestServeQuestionCancelByEmptyAnswers`: the result `{"answers":{}}` gives `OutcomeCancelled`, and the tool output from `ask.Tool.Run` with this thread as the `Asker` is exactly `TextCancelled`. The result `{"answers":{"q1":{"answers":[]}}}` gives `TextSkipped` in the same test. The first check fails on an implementation that builds an answered reply from the empty map, which prints `TextSkipped`.
  - `TestServeQuestionOtherErrorWaitsForOtherWindow`: a `-32603` from one of two clients keeps it open; with one client it gives `OutcomeNoClient`.
  - `TestServeQuestionDeclinedWhenWindowLeaves`.
  - `TestServeQuestionReplayedToLateWindow`.
  - `TestServeQuestionInterruptedResolution`.
  - `TestServeQuestionUnreadableAnswer`.

All fail before this step because the method is not sent.

Docs: `docs/spec/protocol.md`:

- `## Server requests`: add a paragraph on `item/tool/requestUserInput` covering the params, the extensions, the response, the cancel `{"answers": {}}`, the other-error rule, first-answer-wins, replay, and the `resolution` member. This step adds only this paragraph. The protocol workstream, Steps 5 and 13 add their own paragraphs to the same section, and this step does not edit them.
- `## Notifications and items`: add one line saying `waitingOnUserInput` comes from the open question, not from a journal event. This step adds only this line. The protocol workstream, Step 2 and the edit-review workstream, Step 6 add their own lines to the same section.

- Lands after: Step 2 (`Thread.AnswerQuestion`, `Status.Questions` and the `QuestionPrompt` type).

Anchor: David Parnas, Claude Shannon.

