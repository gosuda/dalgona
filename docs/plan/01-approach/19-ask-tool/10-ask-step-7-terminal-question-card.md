#### Ask step 7. Terminal question card

Update the existing implementation in `internal/dal/tui` under the current [question contract](../../../dal/spec/tui.md#questions).
The file and handler outline below identifies responsibilities. The shared input contract controls validation and correction behavior.

Files:

- `internal/tui/question.go` (new). Answer text is typed in the composer, not in the card.
  - `type questionCard struct{ threadID string; params rpc.ToolRequestUserInputParams; answer chan requestAnswer; index int; cursor []int; picked [][]bool; typed []string }`.
  - `newQuestionCard(threadID string, params rpc.ToolRequestUserInputParams, answer chan requestAnswer) questionCard`.
  - `type cardAction int` with `cardStay`, `cardSubmit`, `cardDismiss`.
  - `(c *questionCard) key(msg tea.KeyPressMsg) cardAction` handles the rows keys only; the composer handles input mode. `rowsKey` uses `navigate(key string) bool` for `up`, `k`, `down`, `j`, `tab`, `right`, `l`, `shift+tab`, `left` and `h`, then `choose(key string) cardAction` for the digits, `space`, `enter`, `s` and `esc`.
  - `(c *questionCard) commitText(text string) cardAction` validates the complete candidate under the shared 2000-scalar quota. Valid text is preserved exactly before advancing. An invalid candidate stays recoverable with its field error and does not advance. `(c *questionCard) leaveInput()` returns to the rows.
  - `(c questionCard) response() rpc.ToolRequestUserInputResponse`: every id is present, with an empty list for no answer.
  - `(c questionCard) view(width, height int) string`.
- Keys in rows mode:
  - Up and down move between rows; the Other row is last.
  - Digits `1` to `n` pick an option. Single-select picks and advances. Multi-select toggles.
  - Digit `n+1` sets input mode `inputAnswer` on the Other row.
  - `space` toggles in multi-select and picks in single-select.
  - `enter` picks and advances in single-select, advances in multi-select, and sets input mode `inputAnswer` on the Other row.
  - `tab`/`right`/`l` go to the next question and `shift+tab`/`left`/`h` to the previous one.
  - `s` submits now.
  - `esc` dismisses.
  - Advancing past the last question submits.
  - A free-text question sets input mode `inputAnswer` at once.
- Input mode `inputAnswer`: the composer holds the text, with label `answer: `. The card sets the mode on entry and clears it on leave.
  - The composer edits the text with its own keys, paste included.
  - `enter` calls `commitText` with the composer value, which clears picks in single-select and advances. A pick clears typed text.
  - `esc` calls `leaveInput`, which returns to the option rows. On a free-text question it dismisses instead.
- View:
  - Line 1: `Question <i> of <n> · <header>` on every card, including `n == 1`.
  - Then the wrapped question text.
  - Option rows: `› ` on the cursor row, `(•)`/`( )` or `[x]`/`[ ]`, `<k>. <label>`, and `  Recommended` on option 1. Descriptions show only in the preview block.
  - The Other row: `<n+1>. Other`, plus `: <typed>`.
  - The preview block has 2 rows: row 1 is the cursor option's description, row 2 its `preview` first line, each prefixed `│ `. A longer preview is cut with `…`.
  - The hint `↑/↓ move · 1-7 pick · Space toggle · Enter choose · Tab next · s submit · Esc dismiss`.
  - Model strings use a safe display projection with source mappings. Display cleanup never replaces an offered option value or user answer. Every line uses the shared terminal width profile.
  - Terminal look (Design record, Colour; Design record, The point and the thread hue; Design record, Components): the card is the Open rail panel (Design record, Terminal panels), and each option row is a Measured ledger row (Design record, Terminal rows). Colours are the Design record, Colour text roles and the thread hue only, and the card paints no background. The `s submit` hint is the card's one point (D18), drawn as a filled tag in the thread hue (Design record, The point and the thread hue). `Recommended` is plain text.
  - Height is `panelRows(panelAsk, height)` of the terminal workstream, Step 21: 13 rows, capped at the window height minus 4 (cross-phase decision 53). The rows are title (1), question (2), options (7), preview (2) and hint (1). When rows are short, the preview drops first, then the question text, then the options scroll with `↑ more` and `↓ more` markers. The title and the hint row never drop. The card is drawn in the dock by `renderView`, never printed; only the answer row goes through `m.commit` (the terminal workstream, Step 14, `internal/tui/rows.go`), never by a direct `tea.Println`.
- `internal/tui/protocol.go`: `decodeQuestion(params jsontext.Value) (rpc.ToolRequestUserInputParams, error)` requires at least one question and a non-empty `id` on each. Its errors start with `decode question:`.
- `internal/tui/model.go`:
  - Add the constant `stateAwaitingInput = "awaiting input"` and no field of its own. The question cards join the decision queue of the terminal workstream, Step 26.
  - `Update` adds no window field. The window height is `m.screen.height`, which the terminal workstream, Step 3 sets.
  - `updateKey` in `internal/tui/update.go` (terminal workstream, Step 1): the queue of the terminal workstream, Step 26, gives the key to its front item. When the front item is a question, `answerQuestionKey` takes it.
  - `tea.PasteMsg` goes to `paste` while the front card is typing. The text is `msg.Content`, as `internal/tui/composer.go:65-66` reads it.
  - `applyRequest` (`:224-236`): branch on `msg.method == "item/tool/requestUserInput"` to `applyQuestion`, which appends the card to the queue of the terminal workstream, Step 26, and calls `setState(stateAwaitingInput)` for the TUI's thread. The notification of the terminal workstream, Step 36, already covers an ask request, so this step returns no command.
  - `answerQuestionKey`, while the front card is in input mode, sends `enter` to `commitText` with the composer value, sends `esc` to `leaveInput`, and passes other keys to the composer:
    - `cardSubmit` sends `requestAnswer{value: card.response()}`;
    - `cardDismiss` sends `requestAnswer{value: rpc.ToolRequestUserInputResponse{}}`, the cancel shape (cross-phase decision 9);
    - after the complete response passes local validation and enters the retained sender, append the local submission row defined below. Keep its submitted values separate from later draft edits.
    - both retain the front card, draft, and response sender until `serverRequest/resolved` determines the outcome.
    - a matching correction warning restores editing and permits a corrected response under the same request ID and original deadline.
    - a stale warning cannot reopen a resolved card. The sender's context survives the initial response and ends with the typed request lifecycle.
  - the status-to-state mapping in `internal/tui/model.go` (`:578-581`, the `case "active"` branch): `waitingOnUserInput` maps to `stateAwaitingInput`.
  - `applyDecodedNotification` (`:273-290`): handle `serverRequest/resolved` only when `Resolution != ""` and both `ThreadID` and `RequestID` identify the same pending question. Close only that card and its sender, without sending another response. Ignore stale, duplicate, and unrelated resolutions without changing another card, its draft, the notice, or the state. Recompute the state from remaining decisions and authoritative thread status; one resolved question does not imply that the thread is running.
  - `renderView` in `internal/tui/dock.go` (terminal workstream, Step 1) draws the front item of the queue of the terminal workstream, Step 26. The question card is that item, so this step adds no draw call.
- Resolution attribution: retain local submission state separately from the authoritative outcome. A matching resolution identifies the outcome but not the winning client. Use an other-window notice only when this lifecycle has never queued a local response. Once any local response has been queued, use the neutral notice without inferring whether it won or lost.
- Notice texts, exact:
  - `answered`: `Question answered in another window` when supported by the attribution rule; otherwise `Question answered`.
  - `cancelled`: `Question dismissed in another window` when supported by the attribution rule; otherwise `Question dismissed`.
  - `interrupted`: `Question closed: the turn was interrupted`
  - `declined`: `Question closed: the agent continues without an answer`
  - `timedOut`: use the current TUI timeout notice and close only after authoritative resolution.
- Local submission rows use the existing append path in `internal/dal/tui/rows.go`. They record local send attempts without claiming network delivery, server acceptance, or a winning client.
  - Allocate a local attempt ordinal starting at 1 when a complete validated response enters the retained sender. A local validation or enqueue failure creates no row and does not consume an ordinal.
  - Append one entry through `m.commit(key, lines)` immediately for that attempt. Each question line is `<header>: Local submission #<n>: <answers joined by "; ">`. An empty answer list uses `skip requested`; an explicit dismissal uses `dismiss requested`. Apply the safe display projection to the recorded source values.
  - The structured identity contains row kind, `threadId`, `turnId`, `itemId`, `requestId`, and the local attempt ordinal. Use the original request's `turnId`, not the thread's current turn when printing. If `m.commit` still takes a string, encode the tuple without collisions rather than joining unescaped identifiers.
  - Deduplicate only the exact same structured identity. Keep the counter and submitted snapshots with that request lifecycle across correction and request replay. A corrected response after a matching warning gets the next ordinal and its own immutable local submission row.
  - A transport write failure follows the existing error or connection-loss path. The row remains a local attempt; do not mark it accepted or silently resend it. Generic resolution does not promote a local row or print the local draft as an accepted answer.
  - A question with no local submission prints no local submission row. A timeout retains the authoritative `Timed out` status row under a separate row kind with the same thread, turn, item, and request identity. Duplicate resolution cannot print it twice.
  - Ordinary recorded tool results keep their existing rendering. Do not infer their winning client from a matching local value or add a question journal event.
  - No waiting notification comes from this step. The notification of the terminal workstream, Step 36 already covers an ask request.

Tests in `internal/dal/tui/question_test.go` use the production model and validated request parameters.
Preserve unaffected card tests. Update the existing immediate-row expectations to the local submission contract and add the input and correction cases below.

- `TestQuestionCardSingleSelect` (`2` gives `{"q1":{"answers":["Postgres"]}}`)
- `TestQuestionCardMultiSelectKeepsOptionOrder`
- `TestQuestionCardOtherText` (the composer holds `Mongo`; `enter` gives `{"q1":{"answers":["Mongo"]}}`)
- `TestQuestionCardFreeText` (free text opens input mode at once)
  Coverage now resides in `TestSecondRequestKeepsTypedAnswer`.
- `TestQuestionCardSubmitNowSkips`
- `TestQuestionCardEscCancelsWithEmptyAnswers` (`{"answers":{}}`)
- `TestQuestionCardClosedByResolvedNotification`
- Resolution tests require both thread and request IDs. A delayed resolution for an older question in the same thread must leave the newer question, draft, sender, notice, and state unchanged.
- Attribution tests cover resolution with and without an outstanding local response. An ambiguous race uses a neutral notice and leaves the local submission label unchanged without printing an accepted-answer row.
- `TestQuestionCardFitsWidthAndHeight` (40 columns, window height 12, so `panelRows` gives 9; a 40-line preview; no line wider than 40 cells and at most 9 lines)
- `TestQuestionCardShrinkOrder`: at 9 rows the preview drops first, then the question text, then the options scroll; the title and the hint row stay.
- Update `TestQuestionCardCommitsAnswerRow`: immediately after submission, assert one `Local submission #1` entry with the complete structured identity, while the card, draft, and sender remain pending. A second commit of that exact attempt adds no entry. A later generic resolution adds no accepted-answer row and proves no winning client.
- Update `TestQuestionAnswerRowsShareCallIDAcrossTurns`: submit in `turn-1`, resolve that request, then submit in `turn-2` with the same item ID and a distinct request ID. Start each lifecycle at attempt 1. Both local submission rows must remain. Assert each entry's complete identity against independent expected fields, including its original turn; counting two rows alone does not prove correct turn identity.
- Correction tests submit attempt 1, apply its matching rejection warning, and submit attempt 2 under the same request ID and deadline. Assert two separately keyed local submission rows with the original and corrected values. Repeated delivery of one local commit must not duplicate it.
- Local validation and enqueue failures produce no local submission row. A write failure after enqueue leaves one explicitly local row, reports the transport failure, and never proves acceptance or resends automatically.
- Preserve `TestQuestionAnsweredResolutionWritesNoRow` for a question with no local send. Update `TestQuestionCardClosedByTimedOutNotification` to check the separate outcome identity; a timeout after a local send keeps its local row and adds one `Timed out` row.
- Display safety tests preserve original labels and answers while preventing terminal control execution.
- Input tests reject an oversized edit whole and accept 2000 supplementary scalars without applying a UTF-16 quota.
- Correction tests keep the card and sender after rejection, then resolve one valid response under the original request ID.
- Composition tests use the shared terminal/IME matrix and prevent commit keys from submitting a second action.
- `TestWaitingOnUserInputState`

Docs: `docs/spec/tui.md`:

- In `## Screen`, the list of states adds `awaiting input`. The sentence that maps a thread status adds `active` with the flag `waitingOnUserInput`, which gives `awaiting input`.
- A new `## Questions` section holds the keys, view, height and notices.
- Under `## Notifications`, `serverRequest/resolved` moves from the list of ignored methods to the list of applied notifications. The edit-review workstream, Step 7 adds its own notifications to the applied list, and this step does not edit them.

- Help. This step adds the card's keys (`up`, `down`, `space`, `enter`, `s`, `esc`) to the key table of the terminal workstream, Step 34, as one row group named `Question`.
- Lands after: Steps 1, 9, 14, 21, 26 and 36 of the terminal workstream, and Step 3 of this workstream.

Anchor: Saul Bass, Kenya Hara.

