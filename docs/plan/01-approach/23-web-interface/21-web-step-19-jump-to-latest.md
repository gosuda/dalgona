#### Web step 19. Jump to latest

- Transcript state. `following` becomes `$state`. Add `unseen = $state(0)`: it counts items appended while `!following` and resets on follow.
- Jump button inline in `Transcript.svelte` (no component: it has one caller).
  - `<button class="jump">`, shown when `running && !following` or `unseen > 0`.
  - Text `Jump to latest`, plus ` · ${unseen} new` when `unseen > 0`.
  - `position: sticky; inset-block-end: var(--space-3)` inside the transcript scroller. At 52rem or narrower it is fixed above the dock.
  - Click scrolls to the end and moves focus to the last row (`tabindex="-1"`) through `handoffFocus`.
- Test. `Transcript.dom.test.ts`, describe `jump to latest`: stub `scrollHeight`, `clientHeight` and `scrollTop` with `Object.defineProperty`, fire `scroll`, expect the button, click it, and expect `scrollTo` to be called and focus on the last row. Fails before: there is no cue (`Transcript.svelte:84-85`).
- Spec (`docs/spec/web.md`). `### Layout` gains the Jump to latest rule.
- Lands after: Step 16 (the rebuilt `Transcript` rows).

Anchor: John Carmack.

