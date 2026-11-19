#### Web step 25. Visible follow-up queue

- New `web/src/lib/components/FollowupQueue.svelte` above the textarea: `<ol aria-label="Queued follow-ups">`. Each entry shows its first line (`overflow-wrap:anywhere`) and two buttons:
  - `Edit`: `session.editFollowup(i)` moves the text into the composer and removes the entry.
  - `Remove`: `session.removeFollowup(i)`.
- Keys. The queue key is Alt+Enter, which the terminal workstream, Step 32 names. Help text: `Enter to ${running ? "steer" : "send"} · Shift+Enter for a new line`, plus ` · Alt+Enter to queue` while `running`.
- Test. `Composer.dom.test.ts`, describe `follow-up queue`:
  - `Alt+Enter queues and the entry is listed`
  - `Edit returns the text to the composer`
  - `Remove drops only that entry`
  - Fails before: only a count is shown (`Composer.svelte:109`).
- Spec (`docs/spec/web.md`). Create `### Composer` with the queue behaviour. Step 27 owns its keys table.
- Lands after: Step 24.

Anchor: Dieter Rams.

