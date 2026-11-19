#### Web step 11. Selected thread without colour

- Markup. `web/src/lib/components/ThreadList.svelte:68-69`: `aria-current={thread.id === activeId ? "true" : undefined}`. Delete `class:selected`.
- CSS.
  - `.thread{border-inline-start:0 solid var(--text);padding-inline-start:calc(var(--space-3) + 2px)}`
  - `.thread[aria-current="true"]{border-inline-start-width:2px;padding-inline-start:var(--space-3);background:var(--tone)}`: a 2 px bar in `text` on the leading edge, plus the `tone` fill (Design record, Components)
  - `.thread[aria-current="true"] .title{font-weight:var(--weight-strong)}`
  - Within `@media (hover: hover)`: `.thread[aria-current="true"]:not(:disabled):hover{background:var(--tone)}`. This stops `app.css:171-175` from overriding the selection.
  - `@media (forced-colors: active){.thread[aria-current="true"]{border-inline-start-color:Highlight}}`
- Thread hue. `web/src/lib/hue.ts` exports `threadHue(id: string): 0 | 1 | 2 | 3`, which is `fnv1a32(id) % 4` (Design record, The point and the thread hue). `fnv1a32` is FNV-1a 32-bit over the UTF-8 bytes of `id`, with offset basis 2166136261 and prime 16777619, using `Math.imul` and `>>> 0`. Each row shows an 8 px dot with `data-hue` and `aria-hidden="true"`; the title carries the text.
- State words in the list. Rows show `StateTag` for every thread: `Working` for `active`, `Waiting` for a thread with a pending request in the `Session` queue (step 23), `Failed` for `systemError`, and `Ready` for an idle thread, or `Done` after a completed turn. The date follows the tag; `activityLabel` (`ThreadList.svelte:30-32`) is deleted.
- Tests.
  - Browser check `web/checks/forced-colors-selection.js`, run with media `forced-colors=active`: the selected row's computed `border-inline-start-width` is `2px`, unselected rows are `0px`, and the title font weights differ. Fails before: `app.css:204-206` sets every button border to ButtonText.
  - `ThreadList.dom.test.ts`: `marks only the open thread with aria-current`.
- Spec (`docs/spec/web.md`). `### Layout`: "The open thread has an inline-start bar and a strong title as well as its fill, so it stays distinct under forced colours."
- Lands after: Steps 8 and 10 (tokens and `StateTag`).

Anchor: Edward Tufte.

