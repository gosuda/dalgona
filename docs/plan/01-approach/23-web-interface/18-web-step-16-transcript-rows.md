#### Web step 16. Transcript rows

- New components in `web/src/lib/components/rows/`: `CommandCard`, `ToolCard` and `DiffCard` take the card rules of step 8. The other rows use text and the `line` token only, with no card border.
  - `MessageRow.svelte`: user rows use plain text with `KeywordText`; agent rows use `Markdown` with `streaming={!item.complete && running}`.
  - `ThinkingRow.svelte`: `<details>`, summary `Thinking`, open while streaming.
  - `CommandCard.svelte`: `$ command`, `StateTag`, `exit N`, and `formatDuration(durationMs)` when present.
  - `ToolCard.svelte`: tool name, arguments as JSON in `<pre>`, and output.
  - `PlanPanel.svelte`: the existing plan section (`Transcript.svelte:194`, the `{#if plan}` block) with plan words.
  - `SubagentBlock.svelte`: children of the open thread from `transcript.childThreadIds`. Each shows its title from `threads` or its id, a `StateTag`, and an `Open` button that calls `session.selectThread(childId)`.
  - `AdvisorNote.svelte`: a `<div>`, not `<aside>`.
- Nested calls on the wire, checked at commit `3c4b6f94`.
  - `eval` runs each nested call with the id `<run>/<n>` (`internal/scripthost/effects.go:61`, `callID`, used at `:342-343`). `RunNestedCall` (`internal/agent/extension.go:87`) passes the call to `runNestedCall` (`extension.go:103`), which calls `admit` (`internal/agent/tools.go:233`) and `execute` (`tools.go:357`).
  - `start` (`internal/agent/tools.go:306`) appends a `ToolStarted` with that id, and `runNestedCall` appends its `ToolCompleted`.
  - The tracker makes an item for every started and completed event, whatever the call id (`internal/rpc/serve_track.go:256` `toolStarted`, `:268` `toolCompleted`). `liveSink.item` (`internal/rpc/serve.go:411`) sends each item. `toolKind` (`internal/rpc/serve_items.go:147-159`) sets the item type: class `exec` gives `commandExecution`, `edit` gives `fileChange`, `remote` gives `mcpToolCall`, and `read`, `agent` and `script` give `dynamicToolCall`.
  - So each nested call reaches the browser as its own item. The `eval` call is a `dynamicToolCall` with `tool` `eval`, because `Eval.Class` is `script` (`internal/script/eval.go:78`).
  - The `Item` struct has no parent member (`internal/rpc/types.go:53`), so the browser cannot link a nested row to its `eval` row from the wire.
- Rule for nested rows. Each nested item is one row, drawn by its item kind: `commandExecution` as `CommandCard`, `fileChange` as the file-change row, `dynamicToolCall` as `ToolCard`. The row sits directly after the `eval` row. The reducer appends items in arrival order (`web/src/lib/reducer.ts:97` `appendItem`, `:260` `itemStarted`). Read from the code and not confirmed by a run: the model writes no items while its `eval` call runs, because it waits for the result, so the nested rows follow the `eval` row. Read from the code and not confirmed by a run: two calls of one turn that run in parallel can interleave their rows, because the wire gives no order link between them (parallel runs: `internal/agent/misc_test.go:301`, `rendezvousAgent`).
- At `high`, the `eval` row is a `ToolCard` with its code arguments as JSON and its output. Each nested row is full, by its kind. Step 18 gives the rows at `medium` and `low`.
- Transcript. `Transcript.svelte` dispatches rows through `const rowComponents: Record<TranscriptItem["kind"], Component<RowProps>>`.
- Data shape.
  - The `userMessage` item gains `turnId: string`, set by `transcriptForThread` (`reducer.ts`) from the turn id and by `itemStarted` from `turnId`.
  - `commandExecution` and `toolCall` items gain `durationMs: number | null`, parsed from item `durationMs`.
- Keywords.
  - `KeywordText.svelte` drops `style:background-image`, `.active`, the keyframe and the gradient (`KeywordText.svelte:65`, `:90-116`). A match renders `<mark class="keyword">`. Its look is the keyword mark of the terminal workstream, Step 10: underline, never colour alone. The rule sets `background: none` and `color: inherit`, because the browser's own style for mark paints a fill.
  - Delete `keywordGradient` (`keywords.ts:45`), the local matcher (spans come from `dalgona/keywords/match` (the protocol workstream, Step 7 adds it; `internal/rpc/serve.go:45` registers only `dalgona/keywords/list` today); its `byteRange` counts UTF-8 bytes, so cut the encoded text, not JavaScript string indices), and its tests (`keywords.test.ts:15-102` for `findKeywords`, `:104-116` for `keywordGradient`).
  - `KeywordDefinition` (`types.ts:227`) drops `hue`, and `parseKeywordsResponse` (`validation.ts:496`, `:515`) stops reading it. `KeywordMatch` and `ArmedWord` in `keywords.ts` drop `hue` too (`keywords.ts:7`, `:16`, `:68`, `:93`).
  - The composer keyword row becomes `<p class="keywords">Keywords: {words}</p>` (fixes the named generic `div` at `Composer.svelte:76`).
- Welcome. The empty state shows the `dalgona` mark (new file `web/src/lib/components/Mark.svelte`, which shows `web/static/dalgona.svg` at 24 px and above and `web/static/dalgona-small.svg` below 24 px, with `role="img"` and `alt` set to `dalgona: a caramel candy stamped with a crescent moon and the rabbit Gona` (Design record, Product marks)), the version from `initialize.userAgent`, the line `Start a thread or pick one from the list.` and a `Start thread` button with the behaviour of Step 13 (`aria-disabled` while a start is in flight, no `disabled`).
- Tests. `rows.dom.test.ts`:
  - `renders each item kind with its state word`
  - `keyword matches carry an underline mark and are listed as words`
  - `subagent rows open the child thread`
  - `rows.dom.test.ts`: `renders an eval call and each nested call as its own row, in arrival order`. Input: the five items of the fixture below. Expected: five rows in input order, labelled `eval`, `todo`, `$ ls`, `$ pwd` and `web_fetch`, each showing only its own item's text. Source: the rule above, which follows `internal/rpc/serve_track.go:256-268` (one item per call id). Fails before: no row exists for nested calls.
    Fixture, all items with status `completed`: a `toolCall` with id `call-1` and name `eval`, with arguments whose key `code` holds two lines, `tools.todo(items = [])` and `tools.web_fetch(url = 'https://example.com')`, and empty output; a `toolCall` with id `run-1/1`, name `todo`, arguments `{"items": []}`, empty output; a `commandExecution` with id `run-1/2`, command `ls`, output `a.txt`, exit code 0; a `commandExecution` with id `run-1/3`, command `pwd`, output `/repo`, exit code 0; a `toolCall` with id `run-1/4`, name `web_fetch`, arguments `{"url": "https://example.com"}`, output `ok`.
  - Fails before: thinking reads `Reasoning`, and keywords carry meaning by colour only.
- Spec (`docs/spec/web.md`). Rewrite `### Transcript` for the rows, the word `Thinking`, the subagent block and keyword marks. A nested call MUST be its own row, placed directly after its `eval` row. Keep the sentences that Steps 10 and 14 add to this section: the state words, and "The transcript is not a live region. One status region announces turn changes and waiting requests." Grep over `docs/spec` finds `glow` only in `docs/spec/tui.md`, so this step makes no keyword-spec edit.
- Lands after: Steps 8, 10, 13, 14 and 15.

Anchor: Italo Calvino.

