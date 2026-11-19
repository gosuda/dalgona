#### Web step 18. Verbosity levels and the review view

- `web/src/lib/verbosity.ts`
  - `export const verbosityLevels = ["low", "medium", "high"] as const`
  - `parseVerbosity(value: unknown): Verbosity`, defaulting to `"medium"`
  - `export interface RowPresentation { readonly mode: "full" | "line" | "tail" | "hidden"; readonly tailLines: number }`
  - `presentRow(item, level, live: boolean): RowPresentation`. The rules are the terminal workstream's, Step 16. The browser applies the same rules to its rows.
  - `groupTools(items, level): readonly TranscriptEntry[]` builds `ToolGroup { readonly items: readonly TranscriptItem[] }` from consecutive `commandExecution`, `fileChange` and `toolCall` items when `level === "low"`.
  - `toolGroupSummary(group): string` builds the summary text of the terminal workstream, Step 16.
  - code-mode test inline in `presentRow` (no helper): an `eval` tool call (the terminal workstream, Step 16).
- Eval and nested calls at each level. These rules apply to the fixture of the Step 16 test.
  - `high`: `presentRow` gives `full` for all five rows.
  - `medium`: `presentRow` gives the `eval` row mode `line`, which shows `Eval · 2 lines of code · Done`. `N` counts the lines of the `code` argument, which is named `code` in `internal/script/eval_test.go:161`. Each nested row keeps its kind rule: `commandExecution` gives `tail` with 10 lines, `fileChange` keeps 60 lines per file, and `dynamicToolCall` gives `full`.
  - `low`: `groupTools` puts the five rows in one `ToolGroup`, because they are consecutive `commandExecution` and `toolCall` items. `presentRow` gives `hidden` for each row in a group. `toolGroupSummary` gives `Ran 2 commands · called 3 tools`. `called` counts each `toolCall` item, the `eval` item included.
  - Review: the `Open` button of the `eval` row shows its full code and full output. The `Open` button of a nested row shows that row's full text.
- Rows. Rows read `presentRow`. `CommandCard` in tail mode shows the last 10 lines and the text `Showing the last 10 of ${n} lines` with button `Show all`, which opens review. Delete `.output{max-height:24rem}` (`Transcript.svelte:327-330`).
- Review. New `web/src/lib/components/ReviewView.svelte`, built on `Panel`, which this step creates. `Panel` takes `{ title: string; open: boolean; onClose: () => void; children: Snippet; footer?: Snippet }` and is a `<dialog>` opened with `showModal`, with `aria-labelledby` set. Focus goes to the first focusable element in the body. Esc closes, and focus returns to the opener. The body has `max-height:calc(100dvh - var(--space-8));overflow-y:auto`, and the footer stays outside the body.
  - It shows the full item or group: full Markdown, full thinking, full output (paged by `dalgona/item/output/read` when `outputSpill` is set), and all diffs.
  - It opens from each row's `Open` button (`aria-label="Open full text"`), and from Ctrl+O when focus is in the transcript or composer, targeting the latest finished entry.
  - Esc closes it and returns focus.
- Session. Reads `verbosity` from `dalgona/settings/read`.
- Tests.
  - `verbosity.test.ts`: one test per rule of the terminal workstream, Step 16.
  - `verbosity.test.ts`: `shows an eval call and its nested calls at each level`. Input: the Step 16 fixture. Expected: at `high`, `presentRow` gives `full` for all five rows. At `medium`, `eval` gives `line`, `todo` and `web_fetch` give `full`, and `ls` and `pwd` each give `tail` with `tailLines` 10. At `low`, all five give `hidden`, and `groupTools` gives one group of five items whose `toolGroupSummary` is `Ran 2 commands · called 3 tools`. Source: the rules above, which follow the terminal workstream, Step 16. Fails before: `presentRow` and `groupTools` do not exist yet.
  - `ReviewView.dom.test.ts`: `Ctrl+O opens the full output of the latest command`.
  - `ReviewView.dom.test.ts`: `opens the full code and output of an eval row`. Input: the Step 16 fixture. Expected: the `Open` button of the `eval` row (`aria-label="Open full text"`) shows both code lines, `tools.todo(items = [])` and `tools.web_fetch(url = 'https://example.com')`. The `Open` button of the `ls` row then shows `$ ls` and `a.txt`. Source: the fixture, and the rule above that the review shows the full item. Fails before: no review opens from a row.
  - Fails before: no levels exist, and output scrolls in a 24rem box with no cue.
- Spec (`docs/spec/web.md`). New `### Verbosity`, which cites the terminal workstream, Step 16 for the rules and the summary text. At `medium` an `eval` row MUST show one line. At `low` its nested calls MUST join the tool group.
- Lands after: Steps 4 and 16 (`Session` and the rows that call `presentRow`).

Anchor: Herbert Simon.

