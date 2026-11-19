#### Terminal step 16. Verbosity levels and reasoning collapse

- `rows.go`: `func (m *model) applyVerbosity(r row, item rpc.Item) row`.
  - `high`: unchanged. Reasoning prints its whole text once, at its end. The eval row at high is its Step 14 row, with label `Eval`, subject the first line of `code`, and the eval output as body lines.
  - `medium`: reasoning becomes one line `Thinking · N lines · ctrl+o`, where N counts the lines of the reasoning text. The `eval` item (`EvalName`, `internal/script/mode.go:11`, class `script`) becomes one line `Eval · N lines of code · <state>`, where `N` counts the lines of its `code` argument and `<state>` is the state word of its row. Command output keeps the last 10 lines after `… N earlier lines`. Diffs keep 60 lines per file, then `… N more lines · ctrl+o`.
  - `low`: reasoning is one line, as at medium. Tool items of a turn are held in `m.live.group` and flushed as one line (`Ran 3 commands · edited 2 files · called 4 tools`) when an agentMessage starts or the turn completes, with key `group:<turnId>:<n>`.
  - A failed item at any level keeps its last 10 lines. A failed item first flushes the open group, then prints its own row, so a group line never hides it. A successful `todo` call is left out of the group count, and a nested call counts like a top-level call.
- Final blocks and tails: reasoning and tool output stream only as a tail. The live row shows the last line of the text, cut with `fit`, and the calls field counts a running tool (Step 11). The item prints once, when it completes, as the row that `applyVerbosity` returns, through `commit`.
- `applyConfig` (Step 9) reads `Config.Verbosity` (dependency P7). The default is `medium`.
- Symbols that change: `applyVerbosity` is new. `m.live.group` is a field of the state struct (Structure). The reasoning and tool rows use `renderRow` (Step 14).
- Tests (fail before): `TestVerbosityLevels` (one item set: low gives one summary line, medium gives 10 output lines plus `earlier lines` and a one-line thinking row, high gives everything). `TestFailedItemKeepsLastLinesAtLow` (kept). `TestReasoningTailThenOneRow`: three reasoning deltas, each one line `a`, `b` and `c`. Expected: `commit` is not called during the deltas, and the live row's free text is `c`. At `item/completed`, one commit with the line `Thinking · 3 lines · ctrl+o` at medium. Source: product decision 6 and the section "Terminal screen design", rule "A stream is written in final blocks".
- Spec: `docs/spec/tui.md`, new section `Verbosity`.
- Reference: omp2 `crates/chat/src/cards/bash.rs:10-12` (`BASH_DEFAULT_PREVIEW_LINES=10`) and `crates/chat/src/thinking.rs:34` (via agent://Omp2Features/report). Taken: a 10-line tail, and thinking collapsed. Left: per-card expansion state.
- Lands after: Steps 3, 9, 11, 14 and 15.
- Anchor: Edward Tufte, Herbert Simon

