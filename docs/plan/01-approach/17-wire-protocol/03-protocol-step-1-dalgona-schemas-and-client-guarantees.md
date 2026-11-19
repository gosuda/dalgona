#### Protocol step 1. Dalgona schemas and client guarantees

- Add `internal/rpc/testdata/dalgona-schema/` with one draft-07 file per message: requests as `<method with / replaced by .>.params.json` and `.result.json`, notifications as `<method>.json`. Cover the existing messages: `dalgona.keywords.list`, `dalgona.thread.tree`, `dalgona.thread.leaf.set`, `dalgona.advisor.status`, `dalgona.advisor.set`, `dalgona.advisor.reset` (params and result each), `dalgona.thread.childSpawned.json`, `dalgona.advisor.note.json`. Shapes from `internal/rpc/types.go:13-19,161-178,290-301` and `docs/spec/protocol.md` methods table. Each later step that adds a `dalgona/` message adds its schema files in the same step (`dalgona.<method with / as .>.params.json` and `.result.json` for a request, `dalgona.<method with / as .>.json` for a notification), so `TestDalgonaMessagesHaveSchemas` keeps passing.
- `internal/rpc/serve_harness_test.go`: in `wireClient.route` (`serve_harness_test.go:257`) validate `dalgona/` notifications against `dalgona-schema/<method>.json` instead of skipping them; in `checkResponse` validate `dalgona/` results against `<method>.result.json`. Add `seqs map[string]uint64` to `wireClient`; every `thread/status/changed` MUST carry `seq` greater than the last one seen for its `threadId`, otherwise `t.Errorf("status seq %d after %d for %s", ...)`.
- Add `wireClient.order []string` (method names in arrival order, under `mu`) for ordering assertions in later steps.
- `internal/rpc/serve_notify.go:22-31`: always write `seq`; `internal/rpc/types.go:212` tag becomes `json:"seq"`.
- New `internal/rpc/schema_dalgona_test.go`: `TestDalgonaMessagesHaveSchemas` walks the `methods` keys with prefix `dalgona/` and a test-local `dalgonaNotifications` list, asserts both schema files exist and compile with `jsonschema.NewCompiler`. Fails before: the directory does not exist.
- `docs/spec/protocol.md`: new section "Client guarantees" (text in Schemas, G1 to G6).

Anchor: Tony Hoare, Claude Shannon.

