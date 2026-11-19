#### Code search step 6. Background build after the uncovered-bytes threshold

Anchor: Herbert Simon, John Carmack

- Files:
  - `internal/tool/search/index.go`: the variable `indexRebuildBytes`, the variables `builds` and `buildWG`, and the functions `startBuild` and `runBuild`.
  - `internal/tool/search/search.go`: `runGrep` (one line of trigger).
  - new `internal/tool/search/index_trigger_test.go`.
  - `docs/decisions/search-index.md`: add the section `## Threshold`.
  - `docs/spec/recovery.md`: add two sentences at the end of the `## Rules` section, after the table: A background index build writes no journal event. A crash during a build leaves one generation directory, which the next publish removes.
  - `docs/spec/search.md`: add the sentences of this step to `## Index`.
- Symbols:
  - `var indexRebuildBytes int64 = 32 << 20`. Step 6 sets its value from the sweep of step 4 (outside check) by decision 27. Until that output exists, the value stays at 32 MiB.
  - `var builds sync.Map`, keyed by the index root. `var buildWG sync.WaitGroup`, which only the tests wait on.
  - `func startBuild(root string, env tool.Env)`: calls `builds.LoadOrStore(root, struct{}{})`. When the key was already present it returns. Otherwise it calls `buildWG.Add(1)` and starts `go runBuild(env.Runner, env.Cwd, root, env.Sandbox)`.
  - `func runBuild(run runner.Runner, cwd, root string, sandbox runner.Sandbox)`: `defer buildWG.Done()`. It takes `start := time.Now()` and calls `createGeneration(root, start)`. It takes a process id from `newProcessID("search-index-")`, then calls `run.Start(context.Background(), runner.ProcessSpec{ID: id, Argv: []string{"@self", "__search-index", root, gen, strconv.FormatInt(start.UnixNano(), 10)}, Dir: cwd, Sandbox: runner.Sandbox{Mode: sandbox.Mode, WritableRoots: []string{dir}, DeniedSockets: sandbox.DeniedSockets}})`, and then `collect(context.Background(), run, id)`. On any error it calls `os.RemoveAll(dir)`, logs at warn level with the message `search: index build failed` and the attributes root and error, using `slog.Default()`, and returns. On success it calls `publishGeneration(root, gen)` and then `removeOtherGenerations(root, gen)`. A failure of either is logged the same way.
  - In `runGrep`, after a successful result: `if ref.root != "" && int64(result.Unindexed) >= indexRebuildBytes { startBuild(ref.root, env) }`. The call returns the result without waiting for the build. A remote runner has no `ref.root`, so it never starts a build.
  - `runGrep` keeps cyclomatic complexity at nine or less (`.golangci.yml:30`). The retry stays in `retryWithoutIndex`, and the trigger stays in `startBuild`.
- Tests that fail before the change (compile failure, because `startBuild`, `runBuild` and `indexRebuildBytes` do not exist yet):
  - `TestBuildStartsAboveThreshold` in `index_trigger_test.go`, skipped unless `runner.Available()` is true. Input: `indexRebuildBytes` set to 1 MiB through a helper that restores it with `t.Cleanup`; the isolated home of step 5; three files `big1.txt`, `big2.txt` and `big3.txt`, each 1 MiB and starting with `needle` and a newline; no `CURRENT`; one grep for `needle` through a recording runner. The recording runner starts `os.Args[0]` with the spec's arguments in `spec.Dir`, replacing `@self` with `os.Args[0]`, collects the output, and returns it through `Read`. It records each `__search-index` start and sends a value on a channel. Expected: `Unindexed` 3145728, which is three times the file size, and one recorded build start whose `Argv[2]` equals the index root. The test waits on the channel with a 10-second timeout and does not sleep. It registers `t.Cleanup(buildWG.Wait)` after its temporary directories. Source: the three file sizes in the test.
  - `TestBuildNotStartedBelowThreshold` in `index_trigger_test.go`. Input: one 1 KiB file and one grep. Expected: no build start. Source: the threshold rule.
  - `TestFailedBuildIsNotStartedAgainInProcess` in `index_trigger_test.go`. Input: the recording runner returns an error for `__search-index`; two grep calls above the threshold. Expected: the first build start arrives on the channel, the second call adds no start, and exactly one start is recorded. Source: the one-build rule.
  - `TestBuildRunsInSearchSandbox` in `index_trigger_test.go`. Input: `env.Sandbox` in workspace write mode, with `WritableRoots` set to the workspace and `DeniedSockets` set to one path; one grep above the threshold. Expected: the build spec has `Sandbox.Mode` workspace write, `WritableRoots` with exactly one entry equal to the generation directory, `DeniedSockets` equal to the env's, and `Dir` equal to `env.Cwd`. Source: decision 9 and `runBuild`.
  - `TestNoBuildForRemoteRunner` in `index_trigger_test.go`. Input: a runner that is not local, one grep above the threshold. Expected: no build start. Source: `tool.IsLocalRunner`.
  - `TestNoBuildWithoutSandbox` in `index_trigger_test.go`. Input: `env.Sandbox` mode `off`, one grep above the threshold. Expected: no build start. Source: `indexDir`.
  - `TestCountedBytesOnlyTriggerBuild` in `index_trigger_test.go`. Input: a valid published index, then one grep whose only uncovered file is a library-rejected file with an unchanged stat, above the threshold. Expected: no build start. Source: decision 13.
  - `TestRunBuildPublishesOnlyAfterExitZero` in `index_trigger_test.go`, which calls `runBuild` directly. Input A: the recording runner returns exit code 1 for the build. Expected: no `CURRENT` and no generation directory. Input B: exit code 0, and no file written. Expected: `CURRENT` names the generation directory. Source: the commit point of this workstream.
  - The tests in this file use separate workspaces and cache roots, and never call `t.Parallel`, because `builds` is process-wide.
- Spec, in the section `## Index` of `docs/spec/search.md`:
  - When a grep call counts at least the threshold of uncovered bytes, Search MUST start one build for the workspace.
  - A workspace MUST have at most one build start in each process, even when that build fails.
  - The search call MUST NOT wait for a build.
  - Deleting the directory `search` under the product home MUST restore the scan, and no other state changes.
- `docs/decisions/search-index.md`, section `## Threshold`: the table from the sweep output of step 4, and the value that decision 27 gives. The numbers come from a measurement (outside check).
- Lands after: Code search step 5. Parent uses the index and retries without it. The sweep output of step 4 (outside check).

