#### Web step 26. Esc interrupt guard

- Composer state. `guardUntil = $state(0)`.
- Esc while running, with focus in the composer or on `body`:
  - If `Date.now() < guardUntil`, call `onInterrupt()` and reset.
  - Otherwise set `guardUntil` to `Date.now()` plus the guard window of the terminal workstream, Step 13, and replace the help line with `Esc again to interrupt`. The help line is `<p id="composer-help">`. It has no live role; the one status region is `NoticeStack`'s (Step 5).
- Esc while idle does nothing.
- During a cross-phase decision 41 rate-limit wait, one Esc interrupts at once, because the wait line names Esc. Otherwise the guard above applies.
- Test. `Composer.dom.test.ts`:
  - `one Esc arms and shows the hint; a second Esc within the window interrupts`
  - `the hint clears after the window` (fake timers)
  - Fails before: there is no Esc path.
- Spec (`docs/spec/web.md`). `### Composer`: the guard and its window (the terminal workstream, Step 13).
- Lands after: Steps 25 and 27.

Anchor: Tony Hoare.

