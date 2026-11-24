# Shared input text

## Invariant

`internal/dal/inputtext` owns text policy for every first-party client. The TUI MUST validate single-line fields, the composer, question answers, and resume eligibility through its `Policy`, `Draft`, `Answer`, `Blank`, `Searchable`, and `CheckRequest` API. The TUI MUST NOT keep its own line-break sets, grapheme-boundary loops, quota counts, or NUL checks.

## Scope

This decision applies to the `internal/dal/inputtext` import by `internal/dal/tui`. It covers the `fieldPolicy` and `composerPolicy` values and the `pasteRefusal` reason notices in `paste_routes.go`, the `Draft`-based `textField` and the editor boundary calls in `input.go`, and the cluster window of single-line fields in `dock.go`. It also covers the history row cut in `pickers.go`, `Answer.Check` and `CheckRequest` in `question.go`, `Searchable` in `resume_search.go`, and the `Blank` guards in `update.go`, `commands.go`, and `login.go`. The text contract is in [input-text.md](../spec/input-text.md).

## Enforcement

`TestFieldPasteRefusalNamesTheReason` and `TestSingleLineFieldsRefuseMultilinePaste` check the shared rejection reasons. `TestChipKeysActOnTheChipBesideTheCaret`, `TestFieldWindowShowsWholeClustersAndEscapes`, and `TestPromptHistoryRowCutsAtAClusterAndEscapesControls` check the shared grapheme boundaries. `TestQuestionAnswerPreservesCanonicalText`, `TestQuestionAnswerOverLimitRefusedWhole`, and `TestQuestionOptionLabelSentByteExact` check the shared answer quota. The corpus tests in `internal/dal/inputtext` pin UAX 18.0.0 segmentation, counts, and eligibility. `go test ./internal/dal/tui/` covers the adopted call sites.

## Rejected alternatives

- A TUI-local line-break set. It lost because the shared set already encodes the I03 separators, so a second set drifts.
- TUI-local grapheme loops over the renderer method. They lost because editing boundaries MUST follow the pinned UAX data, not the width profile.
- Per-call-site quota counts. They lost because one `Answer` policy keeps the 2000-scalar quota identical for typing, paste, and response validation.
