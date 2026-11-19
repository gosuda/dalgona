#### Checks of the protocol workstream

1. `just verify` after every step.
2. `go test ./internal/rpc ./internal/agent/... ./internal/config ./internal/keywords ./internal/tool/search ./internal/tool/patch ./internal/app/... ./internal/runner -count=1`.
3. `go test ./internal/rpc -run 'TestDalgonaMessagesHaveSchemas|TestDurationsComeFromTheJournal|TestTokenUsage|TestFileChangeItemCarriesUnifiedDiff|TestEditApprovalSendsTheDiffFirst|TestSpilledOutput|TestKeywordMatch|TestSetting|TestBatchWrite|TestModelList|TestAPIKey|TestSandboxChoiceOverTheWire|TestReviewStart|TestMCPServerStatusList|TestCommandExec|TestFs|TestFuzzyFileSearch' -count=1 -v`.
4. End to end over the real daemon and the stdio bridge (no credentials):

```sh
export DALGONA_HOME=/tmp/dgp LOCAL_LLM_KEY=unused
rm -rf "$DALGONA_HOME" && mkdir -p "$DALGONA_HOME"
printf 'model = "local/qwen-test"\n[providers.local]\nbase_url = "http://127.0.0.1:18080/v1"\napi_key_env = "LOCAL_LLM_KEY"\n' > "$DALGONA_HOME/config.toml"
go build -o /tmp/dalgona ./cmd/dalgona && (/tmp/dalgona daemon &) && sleep 1
{ printf '%s\n' \
 '{"id":1,"method":"initialize","params":{"clientInfo":{"name":"e2e","version":"0"}}}' \
 '{"id":2,"method":"dalgona/keywords/match","params":{"text":"ultrathink please"}}' \
 '{"id":3,"method":"fuzzyFileSearch","params":{"query":"serve_track","roots":["'"$PWD"'"]}}' \
 '{"id":4,"method":"dalgona/settings/read"}' \
 '{"id":5,"method":"command/exec","params":{"command":["git","--version"]}}' \
 '{"id":6,"method":"fs/getMetadata","params":{"path":"'"$PWD"'/go.mod"}}' \
 '{"id":7,"method":"mcpServerStatus/list"}'; sleep 2; } | /tmp/dalgona app-server --listen stdio:// | tee /tmp/dgp.out
```

Expected: id 2 `summary` is `effort max, this turn`; id 3 first `path` is `internal/rpc/serve_track.go`; id 4 lists seven settings with defaults; id 5 `exitCode` 0 and `stdout` starting `git version`; id 6 `isFile` true; id 7 `{"data":[]}`; no `-32601`. Stop the daemon afterwards. `dalgona daemon` takes no arguments and runs in the foreground (`cmd/dalgona/daemon.go:44-46`, `cmd/dalgona/main.go:95`).
5. Sandbox end to end with the test stub (no Landlock needed): `go test ./internal/rpc -run TestSandboxChoiceOverTheWire -count=1 -v` shows `warning`, the question, the follow-up approval, and `dalgona/sandbox/chosen`.

