#### Terminal step 27. Ask card

- The question card is the ask workstream, Step 7 (`internal/tui/question.go`: `questionCard`, `decodeQuestion`, `answerQuestionKey`). That step owns its keys, view and tests. This step adds only the sandbox choice and the queue rule. No file `internal/tui/ask.go` exists, and the names `decodeAsk`, `askRequest`, `askQuestion` and `askOption` are not used.
- Sandbox choice: the sandbox question is an ordinary ask card. It arrives as `item/tool/requestUserInput` with the header `Sandbox` and the options `Accept` and `Dangerously accept` (the protocol workstream, Step 13; cross-phase decision 29). The card renders it by the rules of the ask workstream, Step 7, and the TUI does not branch on it. A cancelled question declines the command, and the next command asks again (cross-phase decision 29).
- Queue and point: the card joins the approval queue (Step 26). One decision shows at a time, the oldest first (cross-phase decision 26). The card's point is set by the ask workstream, Step 7, because one view holds one point (Design record, The point and the thread hue).
- Size: the card is `panelAsk`, 13 rows (`panelRows(panelAsk, height)`, cross-phase decision 53). The rows inside it and their shrink order belong to the ask workstream, Step 7.
- Other text: The input line holds the answer in mode `inputAnswer` (Step 21), which the ask workstream, Step 7 sets. The card holds no text field, and its keys belong to that step.
- No tab line and no review page: the ask workstream, Step 7 owns the view.
- Symbols that change: none beyond `questionCard` (the ask workstream, Step 7) and `panelAsk` (Step 21).
- Failing-first tests:
  - `TestAskDockSteady` (new). Input: `assertSteadyDock` with 50 `down` presses in option rows, then 50 printable keys in answer mode. Expected: 16 frame rows (3 plus 13). Source: Step 21 table.
  - `TestSandboxChoiceIsAskCard` (new). Input: an `item/tool/requestUserInput` request with the header `Sandbox` and the options `Accept` and `Dangerously accept`, then `esc`. Expected: the card opens, the response is `{answers:{}}` (cross-phase decision 9), and the command is declined (cross-phase decision 29).
- Spec: `docs/spec/tui.md`, section `Questions`, owned by the ask workstream, Step 7. This step adds one sentence there: the sandbox choice is an ask card.
- Reference: oh-my-pi `packages/coding-agent/src/modes/components/overlays/ask-dialog.ts:20-37, 504-520` (via agent://OhMyPiFeatures/report). Taken: an Other row. Left: notes, hide, attachments. Tabs and the Review tab are dropped, because the ask workstream, Step 7 has no review page.
- Lands after: Steps 21 and 26, and the ask workstream, Step 7. Step 27 lands after Step 36 because ask Step 7 calls `notify` (Step 36).
- Anchor: Milton Erickson

