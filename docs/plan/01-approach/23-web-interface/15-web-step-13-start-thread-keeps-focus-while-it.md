#### Web step 13. Start thread keeps focus while it works

- Button. `ThreadList.svelte:41` becomes `<button class="primary" aria-disabled={starting ? "true" : undefined} onclick={() => { if (!starting && connected) onNew(); }}>Start thread</button>`.
  - New prop `starting: boolean`. `disabled` now applies only while not connected.
  - `App.svelte` passes `starting={session.startingThread}`.
- Test. `ThreadList.dom.test.ts`: `keeps focus on Start thread while a thread starts`. Fails before: `disabled` drops focus (`+page.svelte:583`).
- Spec (`docs/spec/web.md`). `### Focus` (created by Step 1) gets one sentence: "Start thread stays focusable and reports `aria-disabled` while a start is in flight."
- Lands after: Steps 1 and 4 (`App.svelte` passes `starting`).

Anchor: Tony Hoare.

