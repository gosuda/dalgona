# llmstub test support

## Invariant

The scripted server in `internal/llmstub` MUST use only the standard library and the repository's JSON packages. It MUST NOT import any product package. A core package MAY import it in its tests only.

## Scope

This decision covers `internal/llmstub`, its command in `internal/llmstub/cmd/llmstub`, and the test file `internal/model/chatcompat/llmstub_test.go`, which imports the package to check the chat-completions decoder against the stub's frames. The depguard rules in `.golangci.yml` do not list `internal/llmstub`, so the new import needs this record.

## Enforcement

`internal/llmstub/llmstub_test.go` checks script parsing, bearer authentication, unmatched requests, once turns, client cancellation of a delay, the model list, and the record line. `internal/model/chatcompat/llmstub_test.go` checks that the chat-completions decoder reads a tool call, a reasoning stream, a `Retry-After` delay, and an error frame from the stub. `just test` runs these tests.

## Rejected alternatives

- An exported test constructor in `internal/model/chatcompat`. It lost because the decoder test would then depend on an exported surface that no product uses, and the stub would remain a second copy of the frame format.
- Fake stream bodies copied into each test file. It lost because every copy has to follow the decoder, and the copies drift when the decoder changes.
