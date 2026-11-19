#### Web step 6. Name only the recovery controls that are on screen

- Failure text. `Session.reportFailure(action: string, error: Error | string, retry: () => void): void` builds text `${action}: ${detail}.`. A detail that already ends in `.` gets no second period.
- Failure actions. While `connection === "connected"` the notice gets action `{ label: "Try again", run: retry }`. Otherwise it gets `{ label: "Reconnect", run: () => void this.connect() }`.
- Socket errors. The `onError` handler (old `+page.svelte:157-163`) notifies `Connection error: ${message}.` with a `Reconnect` action.
- Connect failure. Notify `Unable to connect to dalgona: ${detail}. Check that dalgona web is still running.` with a `Reconnect` action.
- Disconnect. When the connection drops after it was connected, show the state word `Offline` in the thread header and the notice `Disconnected from dalgona.` with a `Reconnect` action.
- Removed literals. Delete `Reconnect and try again.` (`+page.svelte:161`, `:230`) and `then select Reconnect` (`:218`). Every caller of `reportFailure` passes its retry closure.
- Test. `session.test.ts`, describe `recovery actions`:
  - `offers Try again for a failed request while connected`, then run it and expect a second call.
  - `offers Reconnect when the connection drops`.
  - Fails before: the text names Reconnect while `StatusBar.svelte:71` hides it.
- Spec (`docs/spec/web.md`). `### Notices` lists the three literal texts and their actions. Step 5 removes the two old error lines from `### Empty states and errors`, so this step deletes nothing.
- Lands after: Steps 4 and 5.

Anchor: Milton Erickson.

