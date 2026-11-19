#### Comment removal step 12. Cursor citations

Anchor: Kenya Hara

- Files: `internal/model/cursor/connect.go`, `internal/model/cursor/cursor.go`, `internal/model/cursor/request.go`, `internal/model/cursor/wire/value.go`, `docs/spec/providers.md`.
- Change: delete the comment lines of rows 58 to 61.
- Spec text, `docs/spec/providers.md`, under `## Subscription and IDE providers`: as a new paragraph after the paragraph that starts `The `cursor` provider MUST open one HTTP/2 Connect stream`: The `cursor` Connect error map MUST follow the Connect protocol error table at `https://connectrpc.com/docs/protocol/#error-codes`. The `cursor` provider MUST take its endpoints, timers, and client version from `senpi`'s `packages/ai/src/api/cursor-agent.ts`. As a new paragraph after the paragraph that starts `The provider MUST answer each blob read`: The `cursor` request code MUST follow `senpi`'s `buildGrpcRequest` and `buildMcpToolDefinitions` in `packages/ai/src/api/cursor-agent.ts`. The `google.protobuf.Value` encoding MUST use the field numbers that `google.protobuf.Value` defines at `https://protobuf.dev/reference/protobuf/google.protobuf/#value`.
- Tests that fail before the change: none. `TestHandAssembledValue` (`internal/model/cursor/wire/value_test.go:82`) pins numbers 2, 5 and 6.
- Check: `just verify` passes.
- Lands after: Comment removal step 11

