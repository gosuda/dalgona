#### Web step 7. Requests waiting on another thread, inside the workspace

- Counts helper. `web/src/lib/labels.ts` adds `export function countLabel(n: number, one: string, many: string): string`, built on the existing `Intl.PluralRules` (`labels.ts:15`). The waiting text has no helper: `NoticeStack` builds it in a `$derived` from the count.
  - `1 request waiting on another thread.` or `2 requests waiting on another thread.`
- Session. Adds `$derived` `waitingElsewhere: { count: number; threadId: string | null }`. `threadId` is the first such request's thread. The notice text is built in place with `countLabel`, in the form below.
- NoticeStack. Renders a persistent `info` row from `waitingElsewhere` with action `Open thread`, which calls `session.selectThread(threadId)`. The row sits inside `<main>`.
- Removed. The `.approval-queue-note` block and its styles (`+page.svelte:636-640`, `:713-717`).
- Test. `web/src/lib/components/App.dom.test.ts`, describe `requests elsewhere`:
  - `shows the waiting note inside main and opens that thread`. Fails before: the note renders after `</div>` of `.shell`.
  - `labels.test.ts` > `countLabel uses singular and plural forms` (contract: English plural rules).
- Spec (`docs/spec/web.md`). `### Approvals`: "Requests from other threads appear as one notice inside the workspace with an Open thread action."
- Lands after: Steps 4 and 5 (`Session` and `NoticeStack`).

Anchor: Christopher Alexander.

