#### Web step 17. Diff card

Anchor: Edward Tufte.

- `web/src/lib/types.ts`: `FileChange` (lines 28-31) gains `readonly additions?: number`, `readonly deletions?: number` and `readonly diffOmitted: boolean`. The parser sets a count only when the wire sends it. `validation.ts` is the only code that builds a `FileChange`, so no other literal changes.
- `web/src/lib/validation.ts`: `fileChanges` (line 244) reads the three members below, next to `path`, `diff` and `kind`.
  - `additions` and `deletions`: when absent, the key is not set. When a non-negative integer, the key is set. Any other value skips the entry, as a wrong `path` or `diff` already does.
  - `diffOmitted`: absent or `false` gives `false`. `true` gives `true`. Any other value skips the entry.
  - Source: the protocol workstream, entry `FileUpdateChange`. A normal change carries both counts. A change with `diffOmitted` `true` has an empty `diff` and no counts.

- `web/src/lib/diff.ts`
  - `export interface DiffLine { readonly kind: "add" | "del" | "hunk" | "meta" | "context"; readonly text: string }`
  - `parseDiff(diff: string): readonly DiffLine[]` classifies by line start. `text` is the whole line, prefix included:
    - `---` and `+++` lines before the first `@@` hunk header are meta. Later they are add or del lines whose text begins `++` or `--`, which the server's unified diff can produce (cross-phase decision 42)
    - `@@` → hunk
    - `+` → add
    - `-` → del
    - anything else → context
  - `parseDiff` counts no lines. The counts come from `change.additions` and `change.deletions` (the section "Prerequisites of the web workstream")
- `web/src/lib/components/DiffCard.svelte`, props `{ change: FileChange; open: boolean }`.
  - Markup: `<details open={open}>`.
  - Summary: `<span class="file-path">` (`min-width:0;overflow-wrap:anywhere`), a kind word, then `+${added} −${removed}` as text. Kind words: `add` → `Added`, `update` → `Changed`, `delete` → `Deleted`.
  - Body: `<pre>` with `<ins>` for add lines, `<del>` for del lines, `<span class="hunk">` for hunk lines and plain text for context. Rows keep the `surface` ground. `<ins>` text is `addedText`, `<del>` text is `failedText` at normal weight (cross-phase decision 34), and hunk lines are `dim`; neither has a text decoration. Prefix characters stay, so colour is not the only signal. At `medium` the card keeps 60 lines.
- Users. `rows/FileChangeRow.svelte` renders one `DiffCard` per change. Rows and diffs are chosen by item kind (`fileChange`), never by tool name. `patch` (`internal/tool/patch/patch.go:24`) and `rewrite` are edit-class tools, and `rewrite`, like `apply_patch`, reaches the browser as a `fileChange` item: `toolKind` maps class `edit` to `fileChange` (`internal/rpc/serve_items.go:147-159`), and the edit unit of the edit-review workstream names `rewrite` as an edit-class tool. The `review` line belongs to the edit-review workstream, Step 8, which lands after this step. A change with `diffOmitted` set to `true` shows the text `Diff not shown.` and no `<pre>`. Its counts show only when `additions` is present (the section "Prerequisites of the web workstream").
- Tests.
  - `diff.test.ts`: `classifies a unified diff`, using the output of `textdiff.Unified` with context 3 (`--- a/<path>`, `+++ b/<path>`, `@@` hunks) for one `apply_patch` edit and one `rewrite` edit. Expected counts are derived by hand from the input.
  - `validation.test.ts`: four tests. Each wraps one `fileChange` item in an `item/completed` notification, as `validation.test.ts:59-70` does, and reads `item.changes`.
    - `reads additions, deletions and diffOmitted of a file change`. Input: one change with `path` `a.txt`, kind `{"type":"update"}`, and a diff of the lines `--- a/a.txt`, `+++ b/a.txt`, `@@ -1 +1 @@`, `-one`, `+uno`, one per line with a final newline, `additions` 1 and `deletions` 1. Expected: `path` `a.txt`, `kind` `update`, the same diff, `additions` 1, `deletions` 1, `diffOmitted` false. Source: the protocol workstream, entry `FileUpdateChange`, where a normal change carries both counts. The counts are 1 and 1 by hand, since `one` is removed and `uno` is added. Fails before: the parser drops both counts.
    - `leaves the counts unset when the server sends none`. Input: the same change without `additions` and `deletions`. Expected, checked with `toStrictEqual`: no `additions` key, no `deletions` key, `diffOmitted` false. Source: the parser rule of this step. Fails before: `diffOmitted` is not set.
    - `reads diffOmitted true with an empty diff and no counts`. Input: `path` `big.bin`, kind `{"type":"update"}`, `diff` empty, `diffOmitted` true. Expected: `path` `big.bin`, `kind` `update`, `diff` empty, `diffOmitted` true, no counts. Source: the protocol workstream, entry `FileUpdateChange`, where `diffOmitted` `true` gives an empty `diff` and no counts. Fails before: `diffOmitted` is not set.
    - `skips a file change whose counts are not integers`. Input: one change with `additions` set to the string `1`. Expected: no changes in the result. Source: the parser rule of this step. Fails before: the change is kept.
  - `diff.test.ts`: `classifies header lines only before the first hunk`. Input: the lines `--- a/x.txt`, `+++ b/x.txt`, `@@ -1 +1 @@`, `--- old`, `+++ new`. Expected in order: `meta` `--- a/x.txt`, `meta` `+++ b/x.txt`, `hunk` `@@ -1 +1 @@`, `del` `--- old`, `add` `+++ new`, each with the whole line as `text`. Source: the header rule of this step. Fails before: `parseDiff` does not exist.
  - `rows.dom.test.ts`: `renders a rewrite change as a file-change row`. Input: one `fileChange` item with one change, `path` `b.txt`, `additions` 1, `deletions` 1, and the diff that `textdiff.Unified` returns for the names `a/b.txt` and `b/b.txt`, context 3, the pre image `alpha` and `beta` on two lines, and the post image `alpha` and `gamma` on two lines (cross-phase decision 42). Copy the returned string into the fixture. Expected: one `DiffCard` whose summary reads `Changed`, `b.txt` and `+1 −1`, with one `<del>` of `-beta` and one `<ins>` of `+gamma`. Source: the counts are 1 and 1 by hand, since `beta` is removed and `gamma` is added. Fails before: the row shows no counts.
  - `DiffCard.dom.test.ts`: `shows counts as text and marks lines with ins and del`.
  - Fails before: the diff is a plain `<pre>` with a 24rem cap (`Transcript.svelte:156`, class `output` capped at `:328`).
- Spec (`docs/spec/web.md`). `### Transcript`: the diff card and its counts. Rows and diffs MUST be chosen by item kind, not by tool name.
- Lands after: Step 16 (the rewritten `### Transcript` and the rows).

