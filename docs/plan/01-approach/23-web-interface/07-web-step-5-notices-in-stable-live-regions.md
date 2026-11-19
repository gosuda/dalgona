#### Web step 5. Notices in stable live regions

- Notice types. `web/src/lib/types.ts` exports:
  - `type NoticeTone = "error" | "warning" | "info"`
  - `interface NoticeAction { readonly label: string; readonly run: () => void }`
  - `interface Notice { readonly id: number; readonly tone: NoticeTone; readonly text: string; readonly actions: readonly NoticeAction[] }`
  - `Session.notify` drops an older notice with the same text and keeps at most 5. It has no helper: one caller, and its test runs on `Session` (step 4).
- Session. Replaces `errorMessage` with `notices: readonly Notice[]` plus methods `notify(tone, text, actions)` and `dismiss(id)`.
- NoticeStack. New `web/src/lib/components/NoticeStack.svelte`.
  - Two regions are always mounted: `<div class="notices" role="alert">` for errors and `<div class="notices" role="status">` for warnings and info. Rows render inside them.
  - Each row is `<p>` with `min-width: 0; overflow-wrap: anywhere`, followed by its action buttons and `Dismiss`. Text colour: `failed-text` in bold (`font-weight: var(--weight-strong)`) for an error, `warning-text` for a warning, `dim` for info (Design record, Colour, cross-phase decision 34).
  - `Dismiss` uses `handoffFocus(() => document.getElementById("prompt") ?? undefined)`.
  - Delete `.error-banner` markup and styles (`+page.svelte:604-609`, `:690-710`).
- Test. `web/src/lib/components/NoticeStack.dom.test.ts`, describe `notice regions`:
  - `keeps the alert region mounted before an error arrives` asserts the same `role=alert` element before and after. Fails before: `+page.svelte:604` inserts the region together with its text.
  - `returns focus to the message field after Dismiss`.
- Spec (`docs/spec/web.md`). Replace the error lines of `### Empty states and errors` with a `### Notices` section: two regions are always mounted, each notice names only the controls it renders, and Dismiss returns focus to the message field. This step creates the page's one status region and one alert region. Steps 14 and 31 announce through them.
- Lands after: Steps 1, 4 and 8 (`handoffFocus`, `Session` and the notice tokens).

Anchor: Kenya Hara.

