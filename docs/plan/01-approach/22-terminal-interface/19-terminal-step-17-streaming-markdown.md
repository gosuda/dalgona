#### Terminal step 17. Streaming Markdown

- `blocks.go`: `type block struct{ text string; code bool }`. `code` is true for fenced lines, which print verbatim and never go through Markdown. `type blockSplitter struct{ open []string; rest string; kind string; afterBlank bool; fence string }`. `open` holds the unfinished block, `rest` the partial last line, `kind` is one of `""`, `para`, `list`, `table` or `quote`, and `fence` holds the marker of an open fence, or is empty.
- `func (s *blockSplitter) Feed(delta string) (final []block, tail string)` appends `delta` to `rest`, then handles each complete line in order by the rules below. `tail` is the last line of `open`, or `rest` when `rest` is not empty, and empty when neither holds text.
- Rules outside a fence, for each complete line L, checked in this order:
  1. Blank (only spaces or tabs): when `kind` is `para`, flush `open` as one final block and reset `kind`. When `kind` is `list`, `table` or `quote`, set `afterBlank`. Otherwise do nothing.
  2. Heading (`#` to `######`, then a space): flush `open`, then emit L as one final block.
  3. Rule (three or more `-`, `*` or `_`, with spaces allowed between): flush `open`, then emit L as one final block.
  4. Fence opener (up to three leading spaces, then three or more backticks or tildes): flush `open`, emit L as a code block, and set `fence` to its marker.
  5. Any other line: when `afterBlank` is set, `kind` is `list`, `table` or `quote`, and L does not belong to that kind, flush `open` first. A line belongs to a list when it starts with a list marker (`-`, `*`, `+`, or digits then `.` or `)`) or has two or more leading spaces. It belongs to a table when it starts with `|`, and to a quote when it starts with `>`. Then L joins `open`, and `afterBlank` is cleared. A block takes its kind from its first line: a list marker gives `list`, `|` gives `table`, `>` gives `quote`, and anything else gives `para`.
- Inside a fence, each complete line is one code block, final at its line end. Such a line never joins `open`. A line whose text is the fence marker, repeated at least as many times as the opener, closes the fence.
- `func (s *blockSplitter) Close() []block` treats `rest` as a last line by the same rules, if it is not empty. Then it flushes `open` as one final block. An open fence closes silently, because its lines are already final.
- `liveItem` gains `split blockSplitter`. `text` keeps the full text for the review (Step 15). `mdLen` and `mdOut` are not added, and `stableMarkdownPrefix` is not written.
- Streaming: `applyDelta` (`model.go:471-487`) for agentMessage calls `item.split.Feed`. Each final block prints with `commit("block:<threadId>:<itemId>:<n>", lines)`, where n counts from 0 per item, and `lines` is the output of `renderBlock`. The tail shows in the live row's free text (Step 11). At `item/completed` for agentMessage, `Close` prints the remaining blocks with the same key form, and the item key `item:<threadId>:<itemId>` is not used for it.
- `markdown.go`: `func (m *model) renderBlock(b block) []string`. A code block returns its text lines unchanged. A prose block returns the lines of `renderMarkdown(b.text)`, which takes its word wrap width from `terminalWidth`.
- Symbols that change: `renderMarkdown` (moved to `markdown.go` by Step 1) uses `terminalWidth` (Step 3), which is one cell short. `renderBlock` and `blockSplitter` are new. `liveItem` gains `split`.
- Tests (fail before): table tests for the splitter, each as input, expected final blocks and expected tail, from the rules above.
  - `TestBlockSplitterParagraph`: Feed `a\nb\n\nc`. Expected final `[{a\nb false}]`, tail `c`. Then Close gives `[{c false}]`.
  - `TestBlockSplitterFenceSpansDeltas`: Feed ```` ```go\nx := 1\n ```` gives final `[{```go true} {x := 1 true}]`, tail empty. Feed ```` `` ```` gives no final block and tail ```` `` ````. Feed ```` `\n ```` gives final `[{``` true}]`, tail empty.
  - `TestBlockSplitterList`: Feed `- a\n- b\n\n` gives no final block and tail `- b`. Feed `text\n` gives final `[{- a\n- b false}]`, tail `text`.
  - `TestBlockSplitterTable`: Feed `| a | b |\n|---|---|\n| 1 | 2 |\n\nnext\n` gives final `[{| a | b |\n|---|---|\n| 1 | 2 | false}]`, tail `next`.
  - `TestBlockSplitterDeltaEndsInsideWord`: Feed `hel` gives no final block and tail `hel`. Feed `lo\n\n` gives final `[{hello false}]`, tail empty.
  - `TestBlockSplitterHeadingAndRule`: Feed `# T\ntext\n---\n` gives final `[{# T false} {text false} {--- false}]`, tail empty.
  - `TestBlockSplitterCloseFlushesPartialLine`: Feed `x`, then Close gives `[{x false}]`.
  - `TestDockHeightStableWhileAnswerStreams`: 200 agent-message deltas at 40x8 and at 100x24. Expected: `len(dockLines())` is equal at every delta. Source: design, "The dock keeps its height".
  - `TestStreamingMarkdownRendersClosedBlocks` (kept, now on the printed blocks and the live row): deltas `# Title\n\nsome text\n\npartial **bo`. Expected: the printed rows contain `Title` without `# `, and the live row's free text is `partial **bo`. Source: design, "A stream is written in final blocks".
- Spec: `docs/spec/tui.md`, one sentence added to Screen (Step 11 owns the section): streamed text prints closed blocks, and the live row shows one tail row.
- Reference: oh-my-pi `packages/tui/src/components/markdown.ts` (via agent://OhMyPiFeatures/report). Taken: the open block stays raw. Left: smooth reveal (omp2).
- Lands after: Steps 3, 9, 11, 14 and 15.
- Anchor: John Carmack

