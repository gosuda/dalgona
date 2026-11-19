#### Web step 4. Move page state into a session

- Session module. New `web/src/lib/session.svelte.ts` exports:
  - `interface RpcPort { call<T>(method: string, params: JsonValue, parse: (v: unknown) => T): Promise<T>; notify(method: string, params: JsonValue): void; respond(id: RequestId, result: JsonValue): void; open(): Promise<void>; close(): void }`. `RpcClient` (`protocol.ts`) already satisfies it.
  - The constructor takes `dial: (url: string, handlers: RpcHandlers) => RpcPort` with no named type.
  - `class Session` with `$state` fields equal to `+page.svelte:58-82` and the `$derived` values of `+page.svelte:83-106`.
- Moved code. These move verbatim as methods: `connect`, `reportFailure`, `loadThreads`, `loadModels`, `loadConfig`, `loadKeywords`, `upsertThread`, `loadAdvisor`, `selectThread`, `readThread`, `createThread`, `sendMessage`, `recordPreview`, `steerMessage`, `queueFollowup`, `interruptTurn`, `changeModel`, `respondApproval`, `sendNextFollowup` (`+page.svelte:116-568`).
- Dispatch. `handleNotification` becomes a lookup in `private readonly effects: { readonly [K in AppNotification["type"]]?: (n: Extract<AppNotification, { type: K }>) => void }`.
- Session entry points.
  - `start(location: Location): void` reads the token through `tokenFromHash` (`protocol.ts`).
  - `stop(): void` bumps the generation and closes the client.
- App component. New `web/src/lib/components/App.svelte` with props `{ session: Session }` holds the template of `+page.svelte:576-646`. `+page.svelte` shrinks to `new Session((url, h) => new RpcClient(url, h))`, `onMount(() => { session.start(location); return () => session.stop(); })` and `<App {session} />`.
- Layout. New `web/src/routes/+layout.svelte` imports `../app.css` (moved from `+page.svelte:3`) and renders `{@render children()}`.
- Test. `web/src/lib/session.test.ts` (unit) with `class FakePort implements RpcPort`. It records calls and answers from a per-method table. Describe `session`:
  - `reconnect keeps queued follow-ups` and `opening another thread discards queued follow-ups`. Contract: the sentence "The resume MUST keep queued follow-ups." under `### Empty states and errors` in `docs/spec/web.md`. Fails before: `Session` does not exist.
- Spec (`docs/spec/web.md`). No behaviour change. Add one sentence to `## Browser client`: the page holds one `Session`.
- Lands after: Step 2 (its `statusSeq` state and `threadStatus` case move into `Session`).

Anchor: David Parnas.

