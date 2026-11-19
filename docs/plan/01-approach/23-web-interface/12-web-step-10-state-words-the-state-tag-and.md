#### Web step 10. State words, the state tag and duration formats

- State words. `web/src/lib/labels.ts` replaces `itemStatusLabels` and `itemStatusLabel` (`labels.ts:49-59`):
  - `export type StateKey = "inProgress" | "awaitingApproval" | "completed" | "failed" | "declined" | "interrupted"`
  - `export const stateWords: Readonly<Record<StateKey, string>>`, with one word for each key. The keys `idle` and `disconnected` join `StateKey`: `idle` gives `Ready`, or `Done` after a completed turn, and `disconnected` gives `Offline`. Each word is the one that cross-phase decision 50 gives.
  - `export function stateKey(status: string): StateKey | null`
  - A thread with the active flag `waitingOnUserInput` maps to `awaitingApproval`, so an open question shows `Waiting` as an open approval does.
  - Thread words are the terminal workstream's words (Step 11; cross-phase decision 50). Plan words are the terminal workstream's words (Step 18).
- Formats. New `web/src/lib/format.ts`:
  - `formatDuration(ms: number): string` (the one form of cross-phase decision 33, which the terminal workstream applies in Steps 11 and 14). The test below pins the boundary values.
  - Percent has no helper: its one caller computes `Math.round(100 * used / window)` inline (Design record, Status band).
- StateTag. New `web/src/lib/components/StateTag.svelte`, props `{ state: StateKey; startedAtMs?: number | null; tick?: number; point?: boolean }`.
  - `inProgress`: `<span class="tag live">` with `<span class="dot" aria-hidden="true">`, the word and `formatDuration(now - startedAtMs)`, in a right-aligned field 7ch wide (cross-phase decision 33). The tag takes the point fill (`.point`) only when `point` is true. Otherwise it is plain text with the dot (Design record, The point and the thread hue).
  - `failed`: `<span class="failed">`, the word `Failed` in bold `failed-text`. It is never a fill (Design record, The point and the thread hue).
  - Any other state: plain text.
  - Rate-limit wait (cross-phase decision 41): while a `dalgona/retry/wait` with `remainingMs` above 0 is open, the `inProgress` tag shows `rate limited · retrying in ${m}m ${ss}s · esc stops` in place of the elapsed time, where `m = Math.floor(remainingMs / 60000)` and `ss = Math.floor(remainingMs % 60000 / 1000)` padded to two digits. It shows the latest `remainingMs` and sits outside every live region. Step 14 announces the wait once.
  - The dot uses `animation: breath var(--motion-breath) ease-in-out infinite` only inside `@media (prefers-reduced-motion: no-preference)`.
  - The counter uses `setInterval(1000)` in an `$effect` only when `matchMedia("(prefers-reduced-motion: reduce)").matches` is false. Otherwise it recomputes when the `tick` prop changes; the session passes its transcript revision.
- Removed. The green Working styles (`StatusBar.svelte:107-109`, `ThreadList.svelte:230-233`, `Transcript.svelte:445-447`).
- Tests.
  - `labels.test.ts`: `uses the state words of this step`.
  - `format.test.ts`, with cross-phase decision 33 values: `0 → "0 ms"`, `999 → "999 ms"`, `1000 → "1s"`, `59999 → "59s"`, `60000 → "1m 00s"`, `3599999 → "59m 59s"`, `3600000 → "1h 00m"`.
  - `StateTag.dom.test.ts`:
    - `only the point is filled, and Failed is bold text`
    - `the elapsed counter ticks every second` (fake timers)
    - `the counter does not tick under reduced motion` (stubbed `matchMedia`)
  - Fails before: the modules are absent and the item word reads `Running`.
- Spec (`docs/spec/web.md`). `### Transcript` and the thread header and status band use the words of the terminal workstream, Step 11. Add `### Motion`: one breathing dot and one counter, both stop under the reduced-motion rule in the section "Decisions of the web workstream".
- Lands after: Steps 8 and 9 (the tokens and the faces).

Anchor: Dieter Rams.

