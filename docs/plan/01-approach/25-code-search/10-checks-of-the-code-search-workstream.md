#### Checks of the code-search workstream

- Focused tests after each step, run by the main agent before `just verify`:
  - Step 1: `go test -run '^$' -bench . -benchmem -count=10 ./internal/tool/search ./internal/tool/read ./internal/tool/snapshot`, and `go test -count=1 -run TestWriteWorkload ./internal/tool/search`.
  - Step 2: `go test -race -count=1 -run TestManifest ./internal/tool/search`.
  - Step 3: `go test -race -count=1 -run 'TestBuild|TestPublish|TestGeneration' ./internal/tool/search`; `go mod tidy` leaves no diff; `GOOS=windows go vet ./internal/tool/search` and `GOOS=freebsd go vet ./internal/tool/search` compile.
  - Step 4: `go test -race -count=1 -run 'TestIndexed|TestCovered|TestRacy|TestSettled|TestUncovered|TestLibrary|TestChanged|TestClock|TestDeleted|TestFind|TestUnindexed|TestMain|TestCorrupt|TestTrigram|TestOutcomeWire' ./internal/tool/search`, and `go test -count=1 -run FuzzTrigramCandidates ./internal/tool/search` on the seed corpus.
  - Step 5: `go test -race -count=1 -run 'TestRun|TestRetry|TestForget|TestCurrent|TestIndexRoot|TestRemote|TestLocal|TestNo|TestSandboxed' ./internal/tool/search`.
  - Step 6: `go test -race -count=1 -run 'TestBuild|TestNoBuild|TestFailedBuild|TestRunBuild|TestCounted' ./internal/tool/search`.
- Mutation probes, each run and then reverted, never committed:
  - Step 4: remove the change-time comparison from `statMatches`. Expected: `TestCoveredFileWithRestoredMtimeIsRead` fails.
  - Step 4: make `decide` skip uncovered files. Expected: `TestIndexedGrepEqualsScan` fails.
  - Step 4: count rejected files in `decide`. Expected: `TestLibraryRejectedFileIsReadNotCounted` fails.
  - Step 5: remove the mode condition from `indexDir`. Expected: `TestNoIndexWithoutSandbox` fails.
  - Step 6: remove the `LoadOrStore` check. Expected: `TestFailedBuildIsNotStartedAgainInProcess` fails.
- Lint: `go tool golangci-lint run ./...`. `cyclop` maximum 9 (`.golangci.yml:30`), `gocognit` 14 (`.golangci.yml:32`), `forbidigo` rejects `log.Print` outside `cmd/` (`.golangci.yml:53-58`), and the `core` depguard rule covers `internal/tool/**` (`.golangci.yml:105`). The new code imports no package from `internal/rpc`, `internal/tui` or the other interface packages.
- Unchanged tests: the list in step 4. The argument-list edits are the only change to existing test code, apart from the fake runner in step 5.
- Parallelism: the index tests never call `t.Parallel`. `builds`, `buildWG` and the library's package variables in `index/write.go` are process-wide.
- Spec style: after Specifications step 40 adds `just spec-style`, every sentence this workstream adds to `docs/spec` must pass it. Before that, each sentence has at most 25 words and no semicolon.
- Formatting: `fmt-check` passes on every new file. `just verify` runs `fmt-check`, `vet`, `lint`, `test`, `vuln`, `hashline-model`, `web-check` and `cf-check` (`justfile:121`).

