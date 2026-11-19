#### Ask step 7. Terminal question card

Files:

- `internal/tui/question.go` (new). Answer text is typed in the composer, not in the card.
  - `type questionCard struct{ threadID string; params rpc.ToolRequestUserInputParams; answer chan requestAnswer; index int; cursor []int; picked [][]bool; typed []string }`.
  - `newQuestionCard(threadID string, params rpc.ToolRequestUserInputParams, answer chan requestAnswer) questionCard`.
  - `type cardAction int` with `cardStay`, `cardSubmit`, `cardDismiss`.
  - `(c *questionCard) key(msg tea.KeyPressMsg) cardAction` handles the rows keys only; the composer handles input mode. `rowsKey` uses `navigate(key string) bool` for `up`, `k`, `down`, `j`, `tab`, `right`, `l`, `shift+tab`, `left` and `h`, then `choose(key string) cardAction` for the digits, `space`, `enter`, `s` and `esc`.
  - `(c *questionCard) commitText(text string) cardAction` stores the composer text, cut to 2000 runes after `sanitizeText`, and advances. `(c *questionCard) leaveInput()` returns to the rows.
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
  - Every model string passes through `sanitizeText` (`render.go:52`); every line goes through `wrapCells` (terminal workstream, Step 3) at `width`.
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
    - both remove the front item from the queue and set `stateRunning`.
  - the status-to-state mapping in `internal/tui/model.go` (`:578-581`, the `case "active"` branch): `waitingOnUserInput` maps to `stateAwaitingInput`.
  - `applyDecodedNotification` (`:273-290`): add `serverRequest/resolved`. Decode `rpc.ServerRequestResolvedNotification`. When `Resolution != ""` and a card for `ThreadID` is open, drop it without answering, set `m.notice` to the notice text for that resolution (see Notice texts below) and set `stateRunning`.
  - `renderView` in `internal/tui/dock.go` (terminal workstream, Step 1) draws the front item of the queue of the terminal workstream, Step 26. The question card is that item, so this step adds no draw call.
- Notice texts, exact:
  - `answered`: `Question answered in another window`
  - `cancelled`: `Question dismissed in another window`
  - `interrupted`: `Question closed: the turn was interrupted`
  - `declined`: `Question closed: the agent continues without an answer`
- Answer row: when the card closes with an answer, dismiss or skip, it writes one row through `m.commit(key, lines)` (`rows.go`, the terminal workstream, Step 14) with key `question:<threadID>:<itemID>`. The lines are one `<header>: <answers joined by "; ">` per question, `<header>: skipped` for a skip, or `<header>: dismissed` for a dismiss. A card closed by another window writes no row. No waiting notification comes from this step. The notification of the terminal workstream, Step 36 already covers an ask request.

Tests in `internal/tui/question_test.go`, package `tui`, using `drive` and `press` (`model_test.go:69-81`). The params come from `schemaValue` so they validate against the vendored schema. All fail before this step because the request is refused.

- `TestQuestionCardSingleSelect` (`2` gives `{"q1":{"answers":["Postgres"]}}`)
- `TestQuestionCardMultiSelectKeepsOptionOrder`
- `TestQuestionCardOtherText` (the composer holds `Mongo`; `enter` gives `{"q1":{"answers":["Mongo"]}}`)
- `TestQuestionCardFreeText` (free text opens input mode at once)
- `TestQuestionCardSubmitNowSkips`
- `TestQuestionCardEscCancelsWithEmptyAnswers` (`{"answers":{}}`)
- `TestQuestionCardClosedByResolvedNotification`
- `TestQuestionCardFitsWidthAndHeight` (40 columns, window height 12, so `panelRows` gives 9; a 40-line preview; no line wider than 40 cells and at most 9 lines)
- `TestQuestionCardShrinkOrder`: at 9 rows the preview drops first, then the question text, then the options scroll; the title and the hint row stay.
- `TestQuestionCardCommitsAnswerRow`: the answer row goes through `commit` once, and a second commit with the same key returns nil.
- `TestQuestionCardStripsEscapes`
- `TestWaitingOnUserInputState`

Docs: `docs/spec/tui.md`:

- In `## Screen`, the list of states adds `awaiting input`. The sentence that maps a thread status adds `active` with the flag `waitingOnUserInput`, which gives `awaiting input`.
- A new `## Questions` section holds the keys, view, height and notices.
- Under `## Notifications`, `serverRequest/resolved` moves from the list of ignored methods to the list of applied notifications. The edit-review workstream, Step 7 adds its own notifications to the applied list, and this step does not edit them.

- Help. This step adds the card's keys (`up`, `down`, `space`, `enter`, `s`, `esc`) to the key table of the terminal workstream, Step 34, as one row group named `Question`.
- Lands after: Steps 1, 9, 14, 21, 26 and 36 of the terminal workstream, and Step 3 of this workstream.

Anchor: Saul Bass, Kenya Hara.

