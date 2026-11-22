#### Checks of the ask workstream

Run after each step: `just verify`.

Focused runs:

- `go test -race -count=1 ./internal/dal/tool/ask/`
- `go test -race -count=1 -run 'Ask|Question|RecoverInterruptsOpenAsk' ./internal/dal/agent/`
- `go test -race -count=1 -run 'Question|AskTool|UserInput' ./internal/dal/rpc/`
- `go test -race -count=1 -run 'AskStays|ScriptCannotCallAsk|ToolModeLists' ./internal/dal/script/`
- `go test -race -count=1 -run TurnBookRefusesQuestion ./internal/dal/app/`
- `go test -race -count=1 -run 'QuestionCard|WaitingOnUserInput' ./internal/dal/tui/`
- `pnpm -C web exec vitest run src/lib/question.test.ts`
- `go tool golangci-lint run ./...` (depguard: no new import crosses a client or battery boundary)

Use [Ask input acceptance](../../../dal/spec/ask.md#input-acceptance-cases) and the [shared corpus](../../../dal/spec/input-text-cases.json) as independent expected values.
Required cases cover blank model fields, strict encoding, exact offered values, source-preserving display, and 2000 versus 2001 scalar answers.
Exercise correction through a production client and daemon connection under the original request ID and deadline.
Race correction with another answer, timeout, interruption, and disconnect. Verify one authoritative outcome and no stale-card revival.
Test CJK and emoji through actual TUI, browser, and GUI input adapters as those implementations land.
Synthetic events do not replace the real IME evidence required by the shared input contract.
These are implementation gates. Documentation checks alone do not prove them.

End to end, with the live-debugging rig of product decision 16 (home `DAL_HOME=/tmp/dq`, `model = "local/qwen-test"`, `[providers.local] base_url = "http://127.0.0.1:18080/v1"`, `api_key_env = "LOCAL_LLM_KEY"`):

1. Script the first response as these frames:

```
data: {"id":"r1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call-q","type":"function","function":{"name":"ask","arguments":"{\"questions\":[{\"header\":\"Database\",\"question\":\"Which database should the service use?\",\"options\":[{\"label\":\"SQLite\",\"description\":\"One file, no server to run.\"},{\"label\":\"Postgres\",\"description\":\"A server with replication.\"}]}]}"}}]}}]}
data: {"id":"r1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}
data: [DONE]
```

   Script the second response as a text frame `done` followed by `data: [DONE]`.
2. Drive `just dal` with omp's `tui` tool and type `pick a database`.
   - The card shows `Question 1 of 1 · Database`, `1. SQLite  Recommended`, `2. Postgres`, `3. Other`, and the word `Waiting` in plain text, with `s submit` drawn as the point.
   - Press `2`.
   - The second request body the server logs must hold a tool message whose content is exactly `User answers:\n- Database (Which database should the service use?): Postgres`.
3. Run `DAL_HOME=/tmp/dq just dal -p "pick a database"`.
   - stderr shows `declined question Database: no interactive client`.
   - The second request carries `declined: no client could answer this question`.
4. Two windows: run `just dalgona` (daemon plus web) and attach the TUI to the same thread with `/resume`.
   - Answer in the TUI.
   - The browser card closes with `Question answered in another window`.
5. Browser check (Design record, Colour; Design record, The point and the thread hue): in the dock card, press Tab through every control. Each focused control shows a 2 px solid `text` ring with a 2 px offset, and `Submit` is the only solid fill, drawn in the thread hue.

