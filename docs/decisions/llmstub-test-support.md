# llmstub test support

## Invariant

The scripted server in `internal/llmstub` MUST use only the standard library and the repository's JSON packages. It MUST NOT import any product package. A core package MAY import it in its tests only.

## Scope

This decision covers `internal/llmstub`, its command in `internal/llmstub/cmd/llmstub`, and the test file `internal/model/chatcompat/llmstub_test.go`, which imports the package to check the chat-completions decoder against the stub's frames. The depguard rules in `.golangci.yml` do not list `internal/llmstub`, so the new import needs this record.

## Enforcement

`internal/llmstub/llmstub_test.go` checks script parsing, bearer authentication, unmatched requests, once turns, client cancellation of a delay, the model list, and the record line. It also checks the 404 path answer, the 405 method answer, and the 401 token answer. `TestLiveModelListAnswersTheOpenAIKeyCheck` in `internal/llmstub/live_login_test.go` reads the model list of the live script with the real `openai` adapter and the `/login` key. It expects `qwen-test` and `qwen-small`, and it expects `http 401` for another key. `internal/llmstub/live_workspace_test.go` holds two tests. `TestLiveWorkspaceFailOutputIsExecutable` checks that `fail-output.sh` has the owner execute bit and that `a.txt` has none. `TestLiveWorkspaceFailOutputFollowsTheT12Contract` runs the script and checks exit status 3, the lines 1 to 30 on standard output, and `boom` on standard error. `TestLiveSessionDriftNamesMissingAndUnrowedTurns` in `internal/llmstub/live_session_test.go` is the negative control of `TestLiveSessionTurnsExist`. It checks that the drift check reports a turn that a row names and the script lacks, and a script turn that no row drives. It also checks that a consistent table reports none. `internal/model/chatcompat/llmstub_test.go` checks that the chat-completions decoder reads a tool call, a reasoning stream, a `Retry-After` delay, and an error frame from the stub. `just test` runs these tests.

## Rejected alternatives

- An exported test constructor in `internal/model/chatcompat`. It lost because the decoder test would then depend on an exported surface that no product uses, and the stub would remain a second copy of the frame format.
- Fake stream bodies copied into each test file. It lost because every copy has to follow the decoder, and the copies drift when the decoder changes.
