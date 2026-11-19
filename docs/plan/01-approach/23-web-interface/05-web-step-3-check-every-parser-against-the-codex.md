#### Web step 3. Check every parser against the Codex schema

- Dependency. devDependency `"ajv": "8.20.0"`. Add a row to the `## Grounded pins` table that Step 1 creates.
- New test file. `web/src/lib/schema.test.ts` (unit project).
  - Load schemas with `import.meta.glob("../../../internal/rpc/testdata/codex-schema/**/*.json", { eager: true, import: "default" })`.
  - Compile with `new Ajv({ strict: false, validateFormats: false })`.
  - Keep a table `cases` of `{ schema: "<file>.json", parse: (p) => unknown, fixture }`. Fixtures hold only Codex members; extension members such as `seq` and `changes` get separate tests.
- Covered methods and files.
  - Notifications (`v2/`):
    - `thread/started` → `ThreadStartedNotification`
    - `thread/status/changed` → `ThreadStatusChangedNotification`
    - `thread/name/updated` → `ThreadNameUpdatedNotification`
    - `turn/started` → `TurnStartedNotification`
    - `turn/completed` → `TurnCompletedNotification`
    - `item/started` → `ItemStartedNotification`
    - `item/completed` → `ItemCompletedNotification`
    - `item/agentMessage/delta` → `AgentMessageDeltaNotification`
    - `item/reasoning/textDelta` → `ReasoningTextDeltaNotification`
    - `item/commandExecution/outputDelta` → `CommandExecutionOutputDeltaNotification`
    - `error` → `ErrorNotification`
    - `turn/plan/updated` → `TurnPlanUpdatedNotification`
    - `serverRequest/resolved` → `ServerRequestResolvedNotification`
  - Requests (top level): `item/commandExecution/requestApproval` → `CommandExecutionRequestApprovalParams`; `item/fileChange/requestApproval` → `FileChangeRequestApprovalParams`.
  - Responses (`v2/`): `ThreadListResponse`, `ThreadStartResponse`, `ThreadResumeResponse`, `ThreadReadResponse`, `TurnStartResponse`, `TurnSteerResponse`, `ModelListResponse`, `ConfigReadResponse`.
  - The test finds each file by title with the glob above, so a title with no file fails the test.
- Tests. Describe `codex schema conformance`.
  - `every fixture is a valid Codex payload` asserts `validate(fixture) === true`. This guards the fixtures.
  - `each parser accepts its valid fixture` checks the parsed value against a hand-stated expected value.
  - `each parser rejects a payload missing a required member`: for each key in the schema's top-level `required`, delete it, expect ajv to return false, and expect the parser to return `null` (notifications) or throw `InvalidPayload` (responses and requests).
  - Fails before: `thread/status/changed` without `status` parses to status `unknown` (`validation.ts:126-129`, `:677-686`). Every other gap the table reveals is fixed in this step.
- Code fix. `validation.ts` adds `function hasMembers(value: { readonly [key: string]: unknown }, keys: readonly string[]): boolean`. Each notification case calls it with the schema's required list. Parsers for requests and responses call `requiredString` or `object` for every required member.
- Spec. In `docs/spec/web.md`, under `## Browser client`, replace the sentence "It speaks the Codex v2 protocol, and `web/src/lib/validation.ts` checks every payload against the shapes in `internal/rpc/testdata/codex-schema`." with: "`web/src/lib/validation.ts` parses each payload the client reads. A parser rejects a payload that lacks a member the vendored Codex schema in `internal/rpc/testdata/codex-schema` requires; `web/src/lib/schema.test.ts` proves this for every parser."
- Lands after: Step 1 (the devDependency and the `## Grounded pins` table).

Anchor: Claude Shannon.

