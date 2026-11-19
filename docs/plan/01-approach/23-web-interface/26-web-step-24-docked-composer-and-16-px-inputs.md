#### Web step 24. Docked composer and 16 px inputs

- Dock. `App.svelte` styles: `@media (max-width: 52rem){.dock{position:sticky;inset-block-end:0;background:var(--surface);border-block-start:1px solid var(--line)}}`.
- Input size. `app.css`: `input,textarea,select{font-size:max(1rem,var(--size-body))}` everywhere. Delete `ModelPicker.svelte:54` (14 px select) and the `max-width:40rem` override (`app.css:195-200`).
- Targets. `button,select{min-height:var(--control-height)}`; `@media (pointer: coarse){button,select{min-height:var(--control-height-coarse)}}`.
- Test. Browser checks:
  - `web/checks/input-size.js` at 375x812: every `input`, `textarea` and `select` has computed font size ≥ 16 px. Fails before: `ModelPicker.svelte:54`.
  - `web/checks/dock.js` at 375x812 with a long transcript scrolled to the top: the `Send` button rect is inside the viewport.
- Spec (`docs/spec/web.md`). `### Layout`: "At 52rem or narrower the dock (notices, request card, composer) stays at the bottom of the viewport. Form text is at least 16px."
- Lands after: Steps 8 and 23 (the `--control-height` token and the dock).

Anchor: Kenya Hara.

