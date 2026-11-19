#### Edit-review step 2. Journal event `tool.judged`
Anchor: Tony Hoare

- Files and symbols:
  - `internal/agent/event/event.go`:
    - `TypeToolJudged = "tool.judged"` after `TypeToolCompleted` (line 20), and `TypeTurnJudged = "turn.judged"` after `TypeTurnCompleted` (line 16), in the const block (lines 12-34).
    - `MaxJudgedPaths = 64` and `MaxJudgeErrorBytes = 1 << 10`.
    - `type ToolJudged struct { CallID, TurnID, By, Verdict string; Paths []string; Scores map[string]float64; Flags, Spoken []string; Feedback, Truncated bool; Parts, FailedParts int; Error string; ElapsedMS int64 }`, placed after the `ToolCompleted` type (line 357) and its methods, with `Type()` and `Validate()`. `Validate` checks `call_id`, `turn_id`, and `by` with `required` (event.go:635), then calls `judgment.validate`. `Parts` is the number of requests the edit unit sent, or 1 when none was sent. `FailedParts` is the number of those requests that failed, or 0. Decoding reads an absent `parts` as 1, and an absent `failed_parts` as 0. `Validate` checks that `Parts` is 1 or more.
    - `type TurnJudged struct { TurnID, By, Verdict string; Paths []string; Scores map[string]float64; Flags, Spoken, Skipped []string; Feedback, Truncated bool; Error string; ElapsedMS int64 }`, placed after the `TurnCompleted` type (line 171) and its methods, with `Type()` and `Validate()`. `Validate` checks `turn_id` and `by` with `required`, then calls `judgment.validate`, then checks `skipped`.
    - `type judgment struct { Verdict string; Paths []string; Scores map[string]float64; Flags, Spoken []string; Feedback bool; Error string; ElapsedMS int64 }` with `func (j judgment) validate() error`. It holds the checks that both events share. The rules are in the Validation list below.
    - `validateScores(scores map[string]float64) error`, moved out of `ApprovalResolved.Validate` (lines 450-454). `ApprovalResolved` and `judgment.validate` both call it, with the same error text: `scores must have named probabilities between zero and one`.
  - `internal/agent/event/encode.go`:
    - `eventMembers` (lines 12-18): add `TypeTurnJudged` to the session case (line 13), and `TypeToolJudged` to the tool case (line 15).
    - `sessionMembers` (line 28): add `case TurnJudged: return turnJudgedMembers(value), nil` after the `TurnCompleted` case (lines 39-41).
    - `toolMembers` (line 124): add `case ToolJudged: return toolJudgedMembers(value), nil` after the `ToolCompleted` case (lines 130-131).
    - `toolJudgedMembers(e ToolJudged) []member` and `turnJudgedMembers(e TurnJudged) []member` write their fields in the order of the JSON in the section "Schemas of the edit-review workstream". They use `arrayValue` with `stringValue`, and `scoresValue` (line 245) for `scores`. They leave out `scores`, `flags`, `spoken`, `skipped`, `feedback`, `truncated`, and `error` when the value is empty or false. `scores` is always left out for `error`. `toolJudgedMembers` writes `parts` only when `Parts` is 2 or more, and writes `failed_parts` after it only when `FailedParts` is 1 or more.
  - `internal/agent/event/codec.go`: `decodeByType` (lines 118-122): add `TypeTurnJudged` to the session case on line 119, and `TypeToolJudged` to the tool case on line 121.
  - `internal/agent/event/decode.go`:
    - `decodeSessionEvent` (line 8): add `case TypeTurnJudged: return decodeTurnJudged(r), nil` after the `TurnCompleted` case (lines 23-24).
    - `decodeToolEvent` (line 75): add `case TypeToolJudged: return decodeToolJudged(r), nil` after the `ToolCompleted` case (lines 81-82).
    - `decodeToolJudged(r *objectReader) ToolJudged` and `decodeTurnJudged(r *objectReader) TurnJudged` call `r.require` on their required fields. They read `paths`, `flags`, and `spoken` through `r.array` (codec.go:232) with `decodeString` (codec.go:277), and `decodeTurnJudged` reads `skipped` the same way. They read `scores` through `r.scores` (codec.go:243), `feedback` and `truncated` through `r.boolean` (codec.go:215), and `elapsed_ms` through `r.int64` (codec.go:198). They follow the pattern of `decodeToolCompleted` (decode.go:182). `decodeToolJudged` reads `parts` through `r.int64` into `Parts`. An absent member gives 1. A present value below 2 fails decoding, because the encoder never writes it. It reads `failed_parts` through `r.int64` into `FailedParts`. An absent member gives 0, and a present value below 1 fails decoding, because the encoder never writes it.
  - `internal/agent/event/state.go`:
    - `State` (line 78): add `JudgedCalls map[string]struct{}` and `JudgedTurns map[string]struct{}` after `CompletedTurns`.
    - `Clone` (line 112): clone both maps after the `CompletedTurns` line.
    - `foldEvent` (lines 173 and 175): add `TypeTurnJudged` to the session case and `TypeToolJudged` to the tool case.
    - `foldSessionEvent` (lines 202-205): add `case TurnJudged: return foldTurnJudged(s, value)` after the `TurnCompleted` case.
    - `foldToolEvent` (lines 283-284): add `case ToolJudged: return foldToolJudged(s, value)` after the `ToolCompleted` case.
    - `foldToolJudged(s *State, e ToolJudged) error`: calls `requireTurn` for `e.TurnID`. It rejects a call still in `PendingCalls` or `InFlight` with `event: fold: tool.judged before tool.completed: <id>`. It rejects a call already in `JudgedCalls` with `event: fold: duplicate tool.judged <id>`. It then records the call id in `JudgedCalls`.
    - `foldTurnJudged(s *State, e TurnJudged) error`: calls `requireTurn` for `e.TurnID`. It rejects a turn already in `JudgedTurns` with `event: fold: duplicate turn.judged <id>`. It then records the turn id in `JudgedTurns`.
    - `completeTurn` (lines 255-268): add `s.JudgedCalls = nil` after `CompletedTurns` is updated, because call ids are checked per turn. `JudgedTurns` is not cleared. `startTurn` already rejects a turn id that is in `CompletedTurns` (lines 244-253), so the once-per-turn check holds for the whole journal.
- Validation (`judgment.validate`, plus the two extra checks):
  - `verdict` is one of `clean`, `uncertain`, `flagged`, `error`.
  - `paths` has 1 to `MaxJudgedPaths` entries, and no entry is empty.
  - `parts` is 1 or more (`ToolJudged` only). The event package does not hold the upper bound of 8, which the agent keeps. `failed_parts` is 0 or more and at most `parts`.
  - `elapsed_ms` is 0 or more.
  - `error`: `Error` is not empty and is at most `MaxJudgeErrorBytes`. `Scores`, `Flags`, and `Spoken` are empty. `Feedback` is false.
  - Any other verdict: `Scores` is not empty and passes `validateScores`. `Error` is empty.
  - `flagged` has one or more `Flags`. `clean` and `uncertain` have none.
  - `Flags` has no repeats, and each entry is a key of `Scores`.
  - `Spoken` has no repeats, each entry is in `Flags`, and `Feedback` is true exactly when `Spoken` is not empty.
  - `TurnJudged` only: each entry of `Skipped` is in `Paths`, with no repeats.
- Invalid cases to test: each must make `Validate` return a non-nil error. Unless marked, a case applies to both events.
  1. `ToolJudged` only: empty `call_id`.
  2. Empty `turn_id`.
  3. Empty `by`.
  4. A verdict that is not one of the four values.
  5. No paths.
  6. 65 paths.
  7. An empty path.
  8. A negative `elapsed_ms`.
  9. A score above 1.
  10. A flag that is not a key of `scores`.
  11. A repeated flag.
  12. `clean` with flags.
  13. `flagged` without flags.
  14. `uncertain` with flags.
  15. `clean`, `uncertain`, or `flagged` without scores.
  16. `error` with scores.
  17. `error` with an empty `error`, or with an `error` over 1 KiB.
  18. A `spoken` id that is not in `flags`.
  19. `feedback` true with empty `spoken`.
  20. `spoken` set while `feedback` is false.
  21. `feedback` on `clean`.
  22. `TurnJudged` only: a `skipped` path that is not in `paths`.
  23. `ToolJudged` only: `parts` of 0 or less.
  24. `ToolJudged` only: `failed_parts` above `parts`, or below 0.
- Failing-first tests (`internal/agent/event`, new `judged_test.go`):
  - `TestToolJudgedRoundTripAndValidation`: encoding the flagged example in the section "Schemas of the edit-review workstream" must give that exact JSON. Decoding it must give a `reflect.DeepEqual` value. Each `ToolJudged` case in the list above must fail `Validate`. `TestToolJudgedPartsRoundTrip` (`internal/agent/event/judged_test.go`): a `ToolJudged` with `Parts` 3 encodes a JSON object with the member `"parts":3`, and decodes to a value that `reflect.DeepEqual` accepts. With `Parts` 1 the JSON has no `parts` member. A JSON `parts` of 1 fails decoding. With `Parts` 3 and `FailedParts` 1, the JSON holds `"parts":3,"failed_parts":1`, and a JSON `failed_parts` of 0 fails decoding. Source: the `parts` and `failed_parts` rules of this step.
  - `TestTurnJudgedRoundTripAndValidation`: the same, with the turn example in the section "Schemas of the edit-review workstream" and each `TurnJudged` case above.
  - `TestFoldToolJudged`: judged while the call is pending fails with `event: fold: tool.judged before tool.completed: <id>`. After `tool.completed` it folds. A repeat fails with `event: fold: duplicate tool.judged <id>`. A `turn_id` that is not the open turn fails. After `turn.completed`, a new turn may judge the same call id. Expected values come from the rules of this step.
  - `TestFoldTurnJudged`: a `turn.judged` for the open turn folds. A second one for that turn fails with `event: fold: duplicate turn.judged <id>`. A `turn.judged` for a turn that is not open fails. After `turn.completed`, the same turn id still fails, because `JudgedTurns` is kept.
- `internal/agent/event/branch.go`: no change. The switch in `treeProjection.add` (starting at branch.go:70) names only `TurnStarted`, `TurnCompleted`, `UserMessage`, and `Branch`. The two new events match no case there, which is correct because neither starts or ends a turn.
- Spec: `docs/spec/events.md`: add the `tool.judged` row directly after the `tool.completed` row, and the `turn.judged` row directly after the `turn.completed` row, in the table under `## Event records`. Insert each row after the whole existing row, whatever its optional-field column reads after the protocol workstream, Step 2. Add "edit calls judged in the open turn" and "turn judged" to the bullet list under `## Fold state` that begins `The state MUST track:`. This step owns those two bullets. The protocol workstream, Step 13 adds its `sandbox.chosen` bullet to the same list. Add these rules to the paragraph that begins with the rule that no turn overlaps another turn: "`tool.judged` MUST name the open turn. It MUST NOT name a call that is pending or in flight, and MUST NOT repeat for a call within a turn. `turn.judged` MUST name the open turn and MUST NOT repeat for a turn id."

