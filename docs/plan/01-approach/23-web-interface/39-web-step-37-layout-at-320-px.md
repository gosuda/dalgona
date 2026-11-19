#### Web step 37. Layout at 320 px

- Sweep all components.
  - Flex children that hold text get `min-width:0`: the `.thread-head` title block of `App.svelte`, `NoticeStack` row text, `.thread-topline`, and `DiffCard` summary.
  - Long text gets `overflow-wrap:anywhere`: titles, previews, paths, model names, notices, the request title and thread references.
  - Thread list `ul{display:grid;grid-template-columns:minmax(0,1fr)}`.
  - Code and output `pre` use `white-space:pre-wrap;overflow-wrap:anywhere`.
- Test. Browser check `web/checks/layout-320.js` at 320x568, using the live-debug scenario with a 300-character unbroken thread title, a 200-character path, a long error detail and a long model id.
  - Pass when `document.documentElement.scrollWidth <= innerWidth`.
  - And every element in `main`, the thread `nav` and the dock has `getBoundingClientRect().right <= innerWidth + 0.5`.
  - Fails before: the missing `min-width:0` (`+page.svelte:701-704`, the model item of `StatusBand.svelte` (Step 20), `ThreadList.svelte:162-221`).
- Spec (`docs/spec/web.md`). In `### Layout`, the sentence "At 320px nothing may clip or scroll sideways." stays. Add: "Long tokens wrap anywhere."
- Lands after: Steps 20, 24 and 29.

Anchor: Christopher Alexander.

