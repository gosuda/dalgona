#### Edit-review step 7. Terminal row
Anchor: Edward Tufte, Otl Aicher

- Files and symbols:
  - `internal/tui/model.go`: in `applyDecodedNotification` (model.go:273), add `case "dalgona/tool/judged": return m.applyToolJudged(msg.params)` and `case "dalgona/turn/judged": return m.applyTurnJudged(msg.params)` beside the `dalgona/advisor/note` case.
  - `applyToolJudged(params)` and `applyTurnJudged(params)` decode `rpc.ToolJudgedNotification` and `rpc.TurnJudgedNotification`. When `m.acceptThread(threadID)` holds (`acceptThread`, model.go:545), each calls `m.printReview(verdict, summary, paths)`. Each returns the decode error of `decodeParams`, as `applyAdvisorNote` does (model.go:376).
  - `printReview(verdict, summary string, paths []string)` decides for both units whether a row prints, so the verbosity rule lives in one place. It calls `m.commit` only when a row prints, and it returns nothing, because `commit` starts no print (the terminal workstream, print rule 1):
    - `clean`: no row.
    - `uncertain`: a row only when the terminal verbosity is `high`.
    - `flagged` and `error`: a row at every verbosity.
    - The row is committed through `commit(key string, lines []string)` in `rows.go`, the terminal workstream, Step 14. The key is `review:<threadId>:<itemId>` for a tool verdict and `review:<threadId>:<turnId>` for a turn verdict. Do not call `tea.Println` directly, so the review row follows the same path as every finished row and is written to scrollback once. Each notification commits at most one row, so one notification prints at most one row.
  - Verbosity values are `low`, `medium`, and `high`, with `medium` as the default. The terminal workstream keeps the value on `screenState` as `verbosity` (the terminal workstream, Step 16). The repository has no verbosity at commit `3c4b6f94`.
  - `internal/tui/render.go`: add `func (m *model) renderReviewRow(verdict, summary string, paths []string) []string`, which returns the lines that `commit` takes. It runs `sanitizeText` (render.go:52) on `summary` and on each path, and joins the paths with `, `. The text is `review  ` plus the summary, plus `  ` plus the paths, with two spaces as cell separators and no middle dot. It wraps the text with `m.screen.wrap(text, terminalWidth(m.screen.width))` (the terminal workstream, Step 2), which keeps every line one cell short. `wrapTerminal` no longer exists, because the terminal workstream, Step 3 deletes it.
  - Styles:
    - `flagged` uses the theme role `roleWarningText` (text role `warning-text`).
    - `error` uses `roleFailedText` in bold, as the failed state word does (Design record, The point and the thread hue).
    - `uncertain` uses `roleDim`.
    - The roles and the `paint` method come from the terminal workstream, Step 9 (`theme.go`). This repository has no `theme.go` yet.
    - No fill. Do not use `faint` (render.go:14), which Design record, Colour limits to panel rails.
- Behaviour and edge cases:
  - A row prints once, in arrival order, like any finished row. A partial review prints the summary that `ReviewSummary` builds, which says that part of the diff was not judged.
  - The row is not in `thread/read` history, because the notification is live only (Step 6). A reloaded thread shows no review row.
  - A notification for another thread prints nothing, through `acceptThread`.
- Failing-first tests (`internal/tui`, `review_test.go`):
  - `TestToolJudgedPrintsFlaggedRow`: a flagged notification at verbosity `medium` gives a non-nil command. `renderReviewRow("flagged", "Flagged: removed code", []string{"a.go"})` contains `review`, `Flagged: removed code`, and `a.go`, in that order.
  - `TestToolJudgedCleanPrintsNothing`: a `clean` notification gives a nil command.
  - `TestReviewRowVerbosity`: a table over verbosity `low`, `medium`, and `high`, the verdicts `flagged`, `error`, `uncertain`, and `clean`, and both methods. Expected: `flagged` and `error` print at all three levels. `uncertain` prints only at `high`. `clean` never prints.
  - `TestReviewRowErrorIsBold`: with colour on, the `error` row contains the bold SGR `\x1b[1m`, and the `flagged` row does not.
- Spec: `docs/spec/tui.md`, under `## Notifications`. Add `dalgona/tool/judged` and `dalgona/turn/judged` to the list of applied notifications in the first sentence of that section. The ask workstream, Step 7 owns the sentence about ignored methods. Add one paragraph that covers the row text, the verbosity rule, one role per verdict, the rule that `clean` prints nothing, and the live-only rule.
- Lands after: Step 6, and the terminal workstream, Steps 9, 14 and 16.

