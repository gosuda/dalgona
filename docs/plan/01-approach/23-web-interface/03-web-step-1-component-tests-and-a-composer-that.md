#### Web step 1. Component tests and a composer that keeps focus

- Dependencies. Add to `web/package.json` devDependencies: `"happy-dom": "21.0.4"`, `"@testing-library/svelte": "5.4.2"`, `"@testing-library/dom": "10.4.2"`, `"@testing-library/user-event": "14.6.9"`. Update `web/pnpm-lock.yaml` with `pnpm -C web install`.
- Vitest config. In `web/vite.config.ts`, import `{ svelteTesting } from "@testing-library/svelte/vite"` and add `svelteTesting()` to `plugins`. Replace `test.include` (`vite.config.ts:19-21`) with `test.projects`:
  - `{ extends: true, test: { name: "unit", environment: "node", include: ["src/lib/**/*.test.ts"], exclude: ["src/lib/**/*.dom.test.ts"] } }`
  - `{ extends: true, test: { name: "dom", environment: "happy-dom", include: ["src/lib/**/*.dom.test.ts"] } }`
- Focus handoff. New `web/src/lib/focus.ts` exports `handoffFocus(fallback: () => HTMLElement | undefined): Attachment<HTMLElement>`, a Svelte attachment.
  - A `focusin` listener records the node as `lastFocused`.
  - On teardown, if `lastFocused === node`, queue a microtask. If `document.activeElement` is then `null` or `document.body`, call `fallback()?.focus()`.
- Composer. In `web/src/lib/components/Composer.svelte`, replace the `{#if running}` block (`Composer.svelte:114-120`):
  - One `<button type="submit">` with text `{running ? "Steer" : "Send"}`. It takes class `point` only when it holds the point (conventions); while a turn runs, the live tag holds it and the button is `quiet`.
  - `Queue follow-up` and `Interrupt` stay inside `{#if running}` and carry `{@attach handoffFocus(() => textarea)}`.
- Test. `web/src/lib/components/Composer.dom.test.ts`, describe `composer focus`:
  - `keeps focus on the submit control when a turn starts`: focus Send, rerender with `running: true`, expect the same element focused with name `Steer`. Fails before: the button is replaced and focus drops to `<body>`.
  - `returns focus to the message field when Interrupt disappears`.
- Spec (`docs/spec/web.md`). New subsection `### Focus` under `## Browser client`: "A control that disappears while focused hands focus to the message field. The submit control keeps its element and changes only its label."
- Decision record. `docs/decisions/web-toolchain.md` gains a `## Grounded pins` table (Package, Pin, Released, Use). Fill the release date from `https://registry.npmjs.org/<package>` field `time["<pin>"]`, and record an OSV `POST https://api.osv.dev/v1/query` result for each pin. Steps 3, 9 and 15 add only their own rows to this table.

Anchor: Tony Hoare.

