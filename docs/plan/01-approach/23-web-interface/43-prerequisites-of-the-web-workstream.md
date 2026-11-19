#### Prerequisites of the web workstream

These must land first, or in the same step order. The interface this workstream assumes from each:

1. Design (`DESIGN.md`, owner's choice):
   - Hex values for every Design record, Colour role in the four concrete themes, the Design record, The point and the thread hue `hues` table, and the Design record, Type for the web; Design record, Components token values. `DESIGN.md` does not exist in the repository. The tests-and-docs workstream, Step 19 writes it from the section "Design record" of this plan.
   - Values for every scale token.
   - The Design record, Product marks files `dalgona.svg` and `dalgona-small.svg` in `assets/brand/`. The web uses no `dal` mark.
   - Steps 8, 9 and 16 cannot finish without them.
2. Protocol workstream:
   - `thread/tokenUsage/updated.tokenUsage.modelContextWindow: int`, present when the context size is known.
   - `Turn.startedAt` (seconds, `internal/rpc/types.go:48`) and `Turn.durationMs` (added by the protocol workstream, Step 2).
   - Item `durationMs` on `commandExecution` and `dynamicToolCall` (added by the protocol workstream, Step 2).
   - `Turn.effort` is not used.
   - `changes` on `item/fileChange/patchUpdated` are Codex `FileUpdateChange` items: `{path, kind: {type: "add"|"update"|"delete"}, diff, additions?, deletions?}`. `changeKind` (`validation.ts:234-241`) reads `kind.type` into `FileChange.kind`. When a side is above 1 MiB, or a blob cannot be loaded, the server sets `diffOmitted` to `true`, `diff` is `""` and `additions` and `deletions` are absent (protocol Schemas; cross-phase decision 42). Example: `{"path":"big.bin","kind":{"type":"update"},"diffOmitted":true,"diff":""}`.
   - Methods the protocol workstream adds. The vendored schema defines them, and `internal/rpc/serve.go` does not register them yet:
     - `fuzzyFileSearch {query, roots, cancellationToken}` → `{files:[{root, path, match_type, file_name, score, indices}]}`
     - `command/exec {command: string[], cwd}` → `{exitCode, stdout, stderr}`
     - `config/batchWrite {edits:[{keyPath, value, mergeStrategy}]}`
     - `review/start {threadId, target}` → `{turn, reviewThreadId}`
     - `dalgona/keywords/match`: keyword spans for a text. The protocol workstream, Step 7 adds it; `internal/rpc/serve.go:45` registers only `dalgona/keywords/list` today.
   - `turn/start` accepts `{type:"image", url:"data:<mime>;base64,..."}` input.
   - Notification `dalgona/retry/wait` (the core-fixes workstream, Step 1a) with params `{threadId: string, turnId: string, remainingMs: number}`. `remainingMs` is the wait left, at most 300 s (cross-phase decision 41). Example: `{"threadId":"t1","turnId":"u1","remainingMs":90000}`. The vendored Codex `ErrorNotification` has `willRetry` but no seconds, so this member is an extension.
3. Settings (the protocol workstream, Steps 8 to 10):
   - `dalgona/settings/read` returns `{settings: [{key, value, defaultValue, options}]}` (the protocol workstream, Schemas). `theme`: `auto|auto-soft|day|soft-day|night|soft-night`, default `auto`.
   - `statusHide` (through `dalgona/settings/read`): array of strings, each an Design record, Status band item id (`effort`, `approval`, `sandbox`, `folder`, `queue`, `context`, `mark`), default `[]`. The server rejects `model` with `status_hide: model cannot be hidden`. Example: `["folder","queue"]`.
   - `editReview`: boolean, default `false`. `icons` is not read by the browser (Design record, Icons).
   - `verbosity`: `low|medium|high`, default `medium`. `approval` comes from the same read. `sandbox` stays on `config/read`.
   - Each `model/list` item carries `presets` (`default`, `smol` or `slow`) and `inputModalities` (Codex field; `image` marks a model that accepts images).
   - Writable through `config/batchWrite` with file key paths `theme`, `verbosity`, `status_hide`, `edit_review.enabled`, `model`, `model_reasoning_effort`, `approval` and `sandbox`.
4. Ask workstream:
   - `item/tool/requestUserInput` params as in `ToolRequestUserInputParams.json` with `isBlocking: true`, `isOther` true on every `ask` question with options, and false on the sandbox choice, extension `multiSelect: boolean` on a question, and extension `preview: string` on an option. The recommended option comes first.
   - Cancel is `{"answers": {}}`.
   - `serverRequest/resolved` carries `resolution`.
   - The sandbox-unavailable choice (product decision 12) and ratchet approvals (product decision 10) arrive as ask requests. The browser question types and parser are declared by the ask workstream, Step 8, which lands after Step 23 of this workstream.
5. Login (the protocol workstream, Step 12, and the core-fixes workstream for device code):
   - `dalgona/account/providers/list`: `{providers: [{id, login: "apiKey"|"browser"|"terminal", signedIn}]}` (the protocol workstream, Schemas).
   - `account/login/start {type: "apiKey", provider, apiKey}`, `{type: "chatgpt"}` and `{type: "chatgptDeviceCode"}`. The browser response carries `authUrl` and `loginId`. The device response carries `loginId`, `userCode` and `verificationUrl`. `account/login/cancel {loginId}` ends a browser flow.
   - `account/login/completed {success, error, provider}`. `loginId` is present for browser and device flows (Codex `AccountLoginCompletedNotification`). `provider` is an extension.
6. Terminal part. Shared rules the web cites and does not copy; the terminal workstream owns each value:
   - state words (Steps 11, 14 and 18), the duration format (cross-phase decision 33, Steps 11 and 14), the tool-group summary text (Step 16)
   - the code-mode predicate (Step 16) and the context-used formula (Step 12)
   - the Esc guard window (Step 13) and the paste-chip threshold (Step 28)
   - the literal-slash rule (`//`) and the slash command list (Steps 7 and 22)
7. Live-debug workstream:
   - `just live-up` starts the scripted model server, the daemon and `dalgona web` in isolated homes and writes the URL to `/tmp/dg/web.url`. `just live-down` stops them.
   - the browser session rows W01 to W30 and the layout matrix probes of the browser session, Step 4.
   - No scenario names. The long-title and forty-thread data have no row in the live-debug workstream, so The checks of this workstream keeps them as manual steps.

