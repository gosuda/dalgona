#### Web step 2. Honour the `seq` guard on thread status

- Parser. `web/src/lib/validation.ts` case `thread/status/changed` (`validation.ts:677-686`) adds `seq: typeof params["seq"] === "number" ? params["seq"] : 0`.
- Type. `web/src/lib/types.ts`, `AppNotification` variant `threadStatus`, adds `readonly seq: number`.
- Reducer. `web/src/lib/reducer.ts` adds `export function acceptThreadStatus(applied: Readonly<Record<string, number>>, threadId: string, seq: number): Readonly<Record<string, number>> | null`.
  - It returns `null` when `seq > 0 && seq < (applied[threadId] ?? 0)`.
  - Otherwise it returns `{ ...applied, [threadId]: Math.max(seq, applied[threadId] ?? 0) }`.
- Page. `web/src/routes/+page.svelte` adds `let statusSeq = $state<Readonly<Record<string, number>>>({})`. Case `threadStatus` in `handleNotification` (`+page.svelte:505-555`) applies the status only when `acceptThreadStatus` is non-null.
- Test. `web/src/lib/reducer.test.ts`, describe `thread status order`. The expected values come from the `seq` sentence under `## Notifications and items` in `docs/spec/protocol.md`.
  - `ignores a status with a lower seq than one already applied`. Fails before: the web drops `seq`.
  - `applies a status without seq`.
  - `keeps one seq per thread`.
- Spec (`docs/spec/web.md`). `### Status bar` gains: "The client keeps the highest applied `seq` per thread and ignores a `thread/status/changed` with a lower one (the rule is the `seq` sentence under `## Notifications and items` in `docs/spec/protocol.md`). Step 20 renames this heading and keeps the sentence."

Anchor: Edsger W. Dijkstra.

