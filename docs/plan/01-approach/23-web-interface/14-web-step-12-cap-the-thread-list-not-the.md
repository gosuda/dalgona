#### Web step 12. Cap the thread list, not the sidebar

- Removed. `:global(.sidebar){max-height:18rem}` (`+page.svelte:733-734`).
- Added. In `ThreadList.svelte`: `@media (max-width: 52rem){nav{max-height:18rem;overflow-y:auto;overscroll-behavior:contain}}`. The `nav` gets `tabindex="0"` and keeps its `aria-label="Conversation threads"` (`ThreadList.svelte:62`), so keyboard users can scroll it.
- Test. Browser check `web/checks/sidebar-cap.js` at 600x900 with 40 threads: the `Start thread` button stays in view after `nav.scrollTop = nav.scrollHeight`, and `nav.clientHeight <= 288`. Fails before: the cap sits on the wrapper.
- Spec (`docs/spec/web.md`). In `### Layout`, replace the sentence "The thread list keeps a maximum height of 18rem." with "At 52rem or narrower the thread list (not the sidebar) keeps a maximum height of 18rem and scrolls."

Anchor: Christopher Alexander.

