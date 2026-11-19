#### Web step 14. One status announcement per turn change

- Transcript markup. In `web/src/lib/components/Transcript.svelte:95`, replace `role="log" aria-label="Thread transcript"` with `<section id="transcript" class="transcript" aria-label="Transcript" tabindex="-1">`. The skip link (`+page.svelte:575`) targets `#transcript`.
- Announcement. `Transcript` computes the string in a `$derived` from the turn status, the error, the waiting request and the cross-phase decision 41 retry wait. It returns:
  - `Working` for a running turn
  - `Done` for a completed turn
  - `Failed: ${error}` for a failed turn
  - `Stopped` for an interrupted turn
  - `Waiting for your reply` while a request waits
  - `Rate limited, retrying` when a cross-phase decision 41 retry wait starts (once)
  - `""` otherwise
- Live region. The announcement shows in the one status region of `NoticeStack` (Step 5). This step mounts no region of its own. The text changes only when its `$derived` inputs change.
- Test. `Transcript.dom.test.ts`:
  - `streamed text is outside every live region`: the message element's `closest('[role=log],[role=status],[role=alert],[aria-live]')` is `null`. Fails before: `role="log"` at `Transcript.svelte:95`.
  - `announces a turn change once` keeps the status text constant across five delta rerenders, then expects `Done`.
- Spec (`docs/spec/web.md`). `### Transcript` adds: "The transcript is not a live region. One status region announces turn changes and waiting requests." `### Layout`: the skip link names the transcript and targets it.
- Lands after: Steps 4, 5 and 10 (the skip link in `App.svelte`, the status region and the state words).

Anchor: Richard Feynman.

