#### Edit-review step 6. Client notification `dalgona/tool/judged`
Anchor: David Parnas

- Files and symbols:
  - `internal/rpc/types.go`: add `ToolJudgedNotification` with the fields `ThreadID string` (`json:"threadId"`), `TurnID string` (`json:"turnId"`), `ItemID string` (`json:"itemId"`), `Verdict string` (`json:"verdict"`), `Summary string` (`json:"summary"`), and `Paths []string` (`json:"paths"`). Add `TurnJudgedNotification` with the same fields except `ItemID`. Place both after `AdvisorNoteNotification` (types.go:295). Neither payload carries a score or `feedback` field, because no client reads them.
  - `internal/rpc/serve_notify.go`: add `func (n ToolJudgedNotification) write(o *out)` and `func (n TurnJudgedNotification) write(o *out)` after `AdvisorNoteNotification.write` (serve_notify.go:171). Each opens the object, writes the strings with `o.str` in field order, writes `paths` with `o.key("paths")`, `o.openArray()`, one `o.text` per path, and `o.closeArray()`, then closes the object (helpers at serve_wire.go:50-63).
  - `internal/rpc/serve_track.go`: add `event.TypeToolJudged: (*tracker).toolJudged` and `event.TypeTurnJudged: (*tracker).turnJudged` to `eventHandlers` (serve_track.go:55-72). Each handler has the shape of `advisorNote` (in `serve_track.go`): `func (t *tracker) toolJudged(_ uint64, ev event.Event)` asserts `event.ToolJudged`, then calls `t.sink.notify("dalgona/tool/judged", func() writer { return ToolJudgedNotification{...} })`. `ItemID` is `e.CallID`, because the tracker keys items by call id (serve_track.go:205). `Summary` is `agent.ReviewSummary(e.Verdict, e.Scores, e.Flags, e.Spoken, e.Error, e.Truncated)`. The turn handler does the same for `event.TurnJudged` with method `dalgona/turn/judged`, `TurnID: e.TurnID`, and no item id. Add the import `github.com/gosuda/dalgona/internal/agent` to serve_track.go. This adds no import cycle, because `internal/rpc` already imports `internal/agent` in serve.go:12 and serve_approval.go:11.
  - `internal/agent/review.go`:
    - Export `func ReviewSummary(verdict string, scores map[string]float64, flags, spoken []string, errText string, truncated bool) string`. It is pure, and both notifications use it. The `truncated` value is the unit's `truncated` field.
    - The label of each question is in the question table of Step 4. A flag id with no label prints as its id.
    - Wording. `ReviewSummary` returns exactly one sentence for each row of the table below. `L` is the labels of the flags for `flagged`, or the labels of the defect questions whose score is at least `reviewUncertainThreshold` and below `reviewFlagThreshold` for `uncertain`. Labels are in table order and are joined with `, `. A flag with no label prints its id. `sound` is never listed. The producers write a truncated `clean` verdict as `uncertain` (Edit-review steps 4 and 4a), so the `clean` row ignores `truncated`. The `error` row ignores `truncated` too. The `uncertain` row with no label and no truncation never occurs in the producers, because an uncertain unit that is not truncated has a score in the band. Each suffix appears at most once in a sentence.

      | Verdict | Label applies | Truncated | Spoken | Text |
      |---|---|---|---|---|
      | `clean` | not applicable | any | not applicable | `No problems found` |
      | `flagged` | yes, the flags are never empty | no | yes | `Flagged: L` |
      | `flagged` | yes | no | no | `Flagged: L (not sent to the model)` |
      | `flagged` | yes | yes | yes | `Flagged: L; part of the diff was not judged` |
      | `flagged` | yes | yes | no | `Flagged: L (not sent to the model); part of the diff was not judged` |
      | `uncertain` | yes | no | not applicable | `Uncertain: L` |
      | `uncertain` | yes | yes | not applicable | `Uncertain: L; part of the diff was not judged` |
      | `uncertain` | no | yes | not applicable | `Uncertain: part of the diff was not judged` |
      | `uncertain` | no | no | not applicable | `Uncertain: no label applies` |
      | `error` | not applicable | any | not applicable | `Failed: ` followed by `errText` cut to 120 bytes, at a character boundary |
    - The server builds the text, so no client parses scores.
  - Schema files. The file naming rule belongs to the protocol workstream, Step 1. This step adds two files to `internal/rpc/testdata/dalgona-schema/`: `dalgona.tool.judged.json` and `dalgona.turn.judged.json`. Each is an object with `additionalProperties: false`, with every field in `required`, so `itemId` is required only in the tool file. `verdict` is an enum of `clean`, `uncertain`, `flagged`, and `error`. `paths` is an array of strings.
  - `internal/rpc/schema_dalgona_test.go` (protocol Step 1): add `dalgona/tool/judged` and `dalgona/turn/judged` to the `dalgonaNotifications` list.
- Behaviour and edge cases:
  - Every verdict sends one notification, `clean` included, because the browser shows every verdict (Step 8).
  - The notification is live only. The history collector's `notify` does nothing (`internal/rpc/serve.go:387`), so `thread/read` carries no verdict.
  - `ReviewSummary` never names `sound`.
- Failing-first tests:
  - `internal/rpc` `serve_judged_test.go` `TestTrackerNotifiesToolJudged`: use a recording sink, thread `thr_1`, and the event `ToolJudged{CallID: "call_7", TurnID: "turn-3", Verdict: "flagged", Flags: ["lost_code"], Scores: {lost_code: 0.94, broken_syntax: 0.02}, Paths: ["internal/model/retry.go"]}`. Expect exactly one notify with method `dalgona/tool/judged`, and the bytes from `newOut()` equal `{"threadId":"thr_1","turnId":"turn-3","itemId":"call_7","verdict":"flagged","summary":"Flagged: removed code (not sent to the model)","paths":["internal/model/retry.go"]}`. The label `removed code` is the edit-review workstream's label for `lost_code`.
  - `TestTrackerNotifiesCleanVerdict`: a `clean` event with no flags. Expect summary `No problems found`.
  - `TestTrackerNotifiesTurnJudged`: the event `TurnJudged{TurnID: "turn-3", Verdict: "uncertain", Scores: {incomplete: 0.62, sound: 0.9}, Paths: ["internal/model/retry.go"]}`. Expect `{"threadId":"thr_1","turnId":"turn-3","verdict":"uncertain","summary":"Uncertain: incomplete","paths":["internal/model/retry.go"]}`. The summary rule above gives this value, and `sound` is excluded.
  - `internal/agent` `TestReviewSummary` (review_test.go), a table with one row for each row of the Wording table. A row that does not say `truncated` set has `truncated` false:
    - `clean`: `No problems found`.
    - `flagged` with flags `[broken_syntax, lost_code]` and spoken `[broken_syntax]`: `Flagged: broken syntax, removed code`.
    - `flagged` with flags `[mystery]` and no spoken: `Flagged: mystery (not sent to the model)`.
    - `uncertain` with `{lost_code: 0.62, placeholder: 0.71, sound: 0.6, broken_syntax: 0.1}`: `Uncertain: removed code, placeholder`.
    - `error` with a 200-byte text: `Failed: ` followed by the first 120 bytes.
    - `clean` with `truncated` set: `No problems found`.
    - `flagged` with flags `[broken_syntax, lost_code]`, spoken `[broken_syntax]`, and `truncated` set: `Flagged: broken syntax, removed code; part of the diff was not judged`.
    - `flagged` with flags `[mystery]`, no spoken, and `truncated` set: `Flagged: mystery (not sent to the model); part of the diff was not judged`.
    - `uncertain` with scores `{broken_syntax: 0.1, sound: 0.2}` and `truncated` set: `Uncertain: part of the diff was not judged`.
    - `uncertain` with scores `{broken_syntax: 0.1, sound: 0.2}` and `truncated` not set: `Uncertain: no label applies`.
    - `uncertain` with scores `{lost_code: 0.62, placeholder: 0.71, sound: 0.6, broken_syntax: 0.1}` and `truncated` set: `Uncertain: removed code, placeholder; part of the diff was not judged`.
    - `error` with a 200-byte text and `truncated` set: `Failed: ` followed by the first 120 bytes.
  - `TestDalgonaMessagesHaveSchemas` (protocol Step 1) passes with the two new files.
- Spec:
  - `docs/spec/protocol.md`, in the table under `## Notifications and items`: put `tool.judged` directly after the `tool.completed` row and `turn.judged` directly after the `turn.completed` row. The protocol workstream, Step 2 and the ask workstream, Step 3 own the other sentences of that section. The rows give the event and its notification: `tool.judged` gives `dalgona/tool/judged {threadId, turnId, itemId, verdict, summary, paths}`, and `turn.judged` gives `dalgona/turn/judged {threadId, turnId, verdict, summary, paths}`.
  - `docs/spec/edit-review.md`, Clients section: the four summary forms and the live-only rule.
- Lands after: Steps 2, 4, 4a and 5, and the protocol workstream, Step 1.

