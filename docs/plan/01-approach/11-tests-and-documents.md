### Workstream: Tests and documents

#### Outcome of the tests-and-docs workstream

Each of the 14 load-sensitive tests now waits on something that actually happens rather than on a time delay: a `testing/synctest` bubble, a socket gate inside the shim, a wait for the shim process to exit, a channel, or a hook on the daemon listener. `go test -race` gives the same result on an idle host and on a loaded one. The hashline documents describe the `patch` tool that ships, and `README.md` describes the `/login` panel that the protocol and terminal steps ship (Step 14). The docs gain a terms list, a supersede rule, "Rejected alternatives" entries, the runner's process-group escape limit and a mutation-probe rule. `AGENTS.md` carries the owner's approved philosophy and points interface work at a new `DESIGN.md`.

#### Decisions of the tests-and-docs workstream

1. Agent tests run in `testing/synctest` bubbles. Rejected: production hooks that expose singleflight waiters or semaphore depth. Reason: `synctest.Wait` already gives "every goroutine is blocked", it is in the standard library, and the repo already uses it (`internal/journal/concurrency_test.go:80`, `internal/model/stream_test.go:38`, `internal/script/scope_test.go:46`).
2. The agent test harness talks to the fake model through an in-memory `http.RoundTripper`, not `httptest.NewServer`. Rejected: building a bare `Host` only for the singleflight test. Reason: a goroutine blocked on a loopback socket is not durably blocked, so `synctest.Wait` would never return in an agent test that calls the fake model (Go source `/usr/lib/go/src/testing/synctest/synctest.go:86-93`, which lists blocking on I/O and system calls as not durably blocking).
3. The shim pause becomes a Unix-socket gate (`<phase>:<socket path>`). Rejected: a FIFO. Reason: a FIFO reader blocks forever if the test never opens the write end, whereas closing a socket listener always frees the shim. A longer duration is not a fix.
4. Tests wait for a shim to exit with `pidfd` plus `waitid(P_PIDFD, WNOWAIT)` on Linux and `kqueue` `NOTE_EXIT` on macOS. Rejected: a reap hook field on `Local`. Reason: it would be production state that exists only for tests, and it misses shims started by another `Local` instance (`internal/runner/local_test.go:20-38`).
5. A new shim phase `group` pauses after `pgid` is written. Rejected: signalling right after `Start`. Reason: the signal test would stop proving that a running command group is signalled.
6. The daemon readiness hook is a package variable, `listenFunc`. It follows the precedent of `writeSpawnPIDFunc` (`internal/runner/local.go:122`). Rejected: adding a ready channel to `Run`'s signature. Reason: that changes the production API for a test.
7. The remote upload stall is held by a channel that the test closes right after `cancel()`. Rejected: `writeDelay`. Reason: a sleep cannot order the cancel before the reply.
8. Waiting without a deadline. No step adds a new wall-clock bound. A defect that blocks forever shows up as the `go test` timeout panic with every goroutine's stack. Positive guards that stay inside a bubble now run on fake time.
9. `approval_stale` is deleted from the spec. Rejected: emitting a code. Reason: the Go tool reports refusals as text. The behaviour already exists as the message `patch: <path> changed while you were approving. Nothing was written. Try again` (`internal/tool/patch/writer.go:89-90`), and no other code in that table is emitted either.
10. The supersede rule lives in the `## Decisions` section of `docs/README.md`. Rejected: a new `docs/decisions/README.md`. Reason: that would be a second index.
11. Edit styles get a new decision file, `docs/decisions/edit-styles.md`. Rejected: putting the alternatives in `hashline-review.md`. Reason: that file is an imported proposal record, and decisions live under `docs/decisions/`.
12. `README.md` describes the `/login` panel after it lands. Step 14 lands after the protocol workstream, Step 12 and the terminal workstream, Step 25, so the text never describes behaviour that has not landed. The protocol workstream, Step 12 does not restate `README.md`. Rejected: an early `README.md` edit, which the protocol change would make stale.
13. "Mode" in the terms list names the per-thread approval and sandbox modes and the configured tool mode. The existing "Mode A" and "Mode B" text in `runners.md` stays. New text names those shapes "thread boxes" and "the daemon in a box".
14. The shim exit wait is synchronous. Rejected: a `shimExited` helper that runs `waitProcessExit` in a goroutine and returns a channel. Reason: both callers wait at once, so the goroutine and channel add work and no ordering.
15. The symbol-rejection check is inlined in the table loop of `TestHashlineRejectsSymbolOperations`. Rejected: keeping a helper with one call site. Reason: a single-use helper adds a name and no reuse.
16. The edit-styles decision names only tests this workstream writes. Rejected: citing a test that checks no description advertises `N*`. Reason: `internal/tool/patch/description.go` has no symbol text, and no step writes that test.
17. Design assets go under `assets/brand/`, and the icon font goes to `web/src/lib/theme/`. Rejected: copying the reference pages into the repository. Reason: cross-phase decision 31 keeps the reference pages out, `DESIGN.md` holds every value, and the web workstream, Step 9 serves the font from `web/src/lib/theme/`.

#### Tests-and-docs step 1. In-memory model transport for the agent harness

- File: `internal/agent/harness_test.go`.
- Add `const fakeModelURL = "http://model.test"`.
- Add `type handlerTransport struct{ handler http.Handler }` with `func (tr handlerTransport) RoundTrip(req *http.Request) (*http.Response, error)`:
  - if `req.Body != nil`, `defer func() { _ = req.Body.Close() }()`;
  - `recorder := httptest.NewRecorder()`, then `tr.handler.ServeHTTP(recorder, req)`;
  - if `req.Context().Err() != nil`, return `nil, req.Context().Err()`;
  - otherwise `response := recorder.Result()`, `response.Request = req`, return it.
- Change `fakeModel` (`:46-55`): replace the field `server *httptest.Server` with `handler http.Handler`.
- Change `newFakeModel` (`:57-66`): set `f.handler = mux` and delete `httptest.NewServer` and `t.Cleanup(f.server.Close)`.
- Change `(*harness).providers` (`:260-270`): `BaseURL: fakeModelURL, Client: &http.Client{Transport: handlerTransport{handler: h.model.handler}}`.
- Why it works: `respond` (`:74-99`) writes the whole body after its gate, so a synchronous transport keeps every existing case. The gate still releases on `r.Context().Done()`.
- Test failing before: none. This is an enabler; `go test -race ./internal/agent/` must stay green.
- Spec: none (test only).
- Anchor: Claude Shannon.

#### Tests-and-docs step 2. `internal/agent/request_models_test.go:173`

- Cause: a 200 ms sleep stands in for "all 8 callers have joined the singleflight call". Under load a late caller misses the flight and calls `Models` a second time.
- Replacement: in `TestProviderModelsCollapsesConcurrentLookups` (`:153-179`), keep `t.Parallel()` on the outer `t` and wrap the rest in `synctest.Test(t, func(t *testing.T) {...})`:
  1. Inside the bubble: `newHarness(t)`, build the lister and registry, start 8 goroutines calling `h.host.providerModels(t.Context(), registry, "stub")`.
  2. Call `synctest.Wait()`. The leader is durably blocked on `<-s.release`, which is a channel created in the bubble. The followers are blocked in singleflight's `WaitGroup.Wait`, which the bubble's `Add` made durable.
  3. Assert `lister.calls.Load() == 1` with the message `agent: provider models calls while 8 lookups wait = %d, want 1`.
  4. `close(lister.release)`, `collectModels`, then keep the final `calls == 1` check.
- Delete the `entered` counter, both `waitFor` calls, the `time.Sleep`, and the `waitFor` helper (`:128-137`, used nowhere else).
- Imports: add `testing/synctest`.
- Mutation probe: in `lookupModels` (`internal/agent/request.go:313-331`), replace `h.modelsGroup.Do(...)` with a direct call to the closure. The test reports 8 calls.
- Spec: none.
- Anchor: Edsger W. Dijkstra.

#### Tests-and-docs step 3. `internal/agent/childcap_test.go:76`

- Cause: a 500 ms window is used to show that no ninth spawn enters `OnChildStarted`. Under load a defect can land after the window closes.
- Replacement: wrap the body of `TestStartChildBoundsConcurrentSpawns` (`:14-35`) in `synctest.Test`, with `t.Parallel()` staying outside.
- Rewrite `expectCapped` (`:70-78`):

  ```go
  synctest.Wait()
  select {
  case <-entered:
      fire()
      t.Fatal("more than 8 concurrent script spawns; StartChild bypasses the children cap")
  default:
  }
  ```

  After `Wait`, the 8 waiting callers are durably blocked on `t.children <-` (`internal/agent/extension.go:120-125`, a channel created in the bubble at `thread.go:118`).
- Imports: keep `time`, which `drainEntries` still uses. Add `testing/synctest`.
- Mutation probe: remove the `select` on `t.children` and the three `<-t.children` lines in `StartChild` (`extension.go:120-144`). The test fails.
- Spec: none.
- Anchor: Tony Hoare.

#### Tests-and-docs step 4. `internal/agent/misc_test.go:309`

- Cause: the 5 s `time.After` in `rendezvousAgent.Run` (`:302-314`) decides between "child" and "alone". On a slow host the partner can arrive after 5 s of real time.
- Replacement: wrap the body of `TestConsecutiveAgentCallsRunTogether` (`:316-331`) in `synctest.Test` and keep the `select` unchanged. Inside the bubble, `time.After` runs on the fake clock. It advances only when every goroutine is durably blocked, which happens only when the partner cannot arrive.
- Mutation probe: in `runAgentBatch` (`internal/agent/tools.go:136-153`, with `group.Go` at line 145), replace `group.Go(func() {...})` with a direct call. The test gets `alone k1`.
- Spec: none.
- Anchor: Richard Feynman.

#### Tests-and-docs step 5. `internal/agent/steer_atomic_test.go:82`

- Cause: a 200 ms negative window is used to show that a rejected steer publishes no trust prompt.
- Replacement: wrap the body of `TestRejectedSteerSkipsWorkspaceTrust` (`:86-104`) in `synctest.Test`.
- Rewrite `requireNoTrustPrompt` (`:77-84`): call `synctest.Wait()`, then a `select` whose `case update := <-updates:` keeps the existing `t.Fatalf` and whose `default:` passes.
- Mutation probe: add `go func() { _ = t.ensureTools(context.WithoutCancel(ctx)) }()` as the first line of `Submit` (`internal/agent/entry.go:26`). The approval update arrives and the test fails.
- Spec: none.
- Anchor: Claude Shannon.

#### Tests-and-docs step 6. `internal/agent/override_test.go:190`

- Cause: a 5 s bound stands in for "Submit does not wait for the tool-set build".
- Replacement: wrap the body of `TestToolSetBuildDoesNotBlockSubmit` (`:217-229`) in `synctest.Test`.
- Rewrite `expectSubmitDuringBuild` (`:181-194`): start the goroutine, call `synctest.Wait()`, then:

  ```go
  select {
  case err := <-submitted:
      if err != nil {
          t.Fatalf("steer during a tool set build: %v", err)
      }
  default:
      s.unblock()
      t.Fatalf("Submit blocked while a tool set was building; after release it returned %v", <-submitted)
  }
  ```

- Mutation probe: delete the `if t.currentTools() != nil { return nil }` early return in `ensureTools` (`internal/agent/thread.go:226-228`). The steer then builds `openai/gpt-slow` and blocks.
- Spec: none.
- Anchor: John Carmack.

#### Tests-and-docs step 7. `internal/runner/local_test.go:410` and `:301`

- Cause at `:410`: `waitForExit` polls every 10 ms against a 5 to 30 s deadline.
- Cause at `:301`: the `startTestProcess` cleanup loops on a 3 s context.
- Add three files:
  - `internal/runner/exitwait_linux_test.go` (`//go:build linux`), `func waitProcessExit(pid int, start string) error`:
    1. `fd, err := unix.PidfdOpen(pid, 0)`; ESRCH returns nil; any other error returns `fmt.Errorf("pidfd_open %d: %w", pid, err)`.
    2. `defer func() { _ = unix.Close(fd) }()`.
    3. If `processStartTime(pid) != start`, return nil.
    4. Loop on `unix.Waitid(unix.P_PIDFD, fd, &info, unix.WEXITED|unix.WNOWAIT, nil)`: ECHILD returns nil, EINTR repeats, anything else returns.
  - `internal/runner/exitwait_darwin_test.go` (`//go:build darwin`), the same function:
    1. `unix.Kqueue()`, then `unix.SetKevent(&change, pid, unix.EVFILT_PROC, unix.EV_ADD|unix.EV_ONESHOT)` and `change.Fflags = unix.NOTE_EXIT`.
    2. Register with `unix.Kevent(kq, []unix.Kevent_t{change}, nil, nil)`; ESRCH returns nil.
    3. Do the start-time check.
    4. Loop on `unix.Kevent(kq, nil, events, nil)` while the error is EINTR.
  - `internal/runner/exitwait_other_test.go` (`//go:build !windows && !linux && !darwin`): return `errors.New("runner: waiting for a shim exit needs pidfd or kqueue")`.
- In `local_test.go`, add `func waitShimExit(local *Local, id string) error`. It calls `readIdentity(filepath.Join(local.dir, id, "pid"))` (`local.go:605`) and returns `waitProcessExit(identity.pid, identity.start)`. Both callers wait at once, so the call is synchronous, with no goroutine or channel.
- Change `waitForExit` to `waitForExit(t *testing.T, local *Local, id string) Output`:
  1. Call `waitShimExit(local, id)` and fail on error with `wait for shim %s: %v`.
  2. Call `local.Read`.
  3. Fail with `shim %s exited without an exit record: %+v` when `Exit == nil`.
- Change the `startTestProcess` cleanup (`:285-303`): one `local.Signal(context.Background(), spec.ID, SignalKill)`; an error that is not `ErrProcessNotFound` goes to `t.Errorf`; then call `waitShimExit(local, spec.ID)` and pass any error to `t.Errorf`.
- Remove the timeout argument at every caller. `grep -rn 'waitForExit(' internal/runner` must then show no call with a duration. The list is:
  - `local_test.go:34, 49, 105, 129, 193, 211, 237, 320, 343`
  - `local_start_cleanup_test.go:21`
  - `execserver_test.go:112`
  - `samebuild_linux_test.go:136`
  - `sandbox_darwin_test.go:59`
  - `sandbox_linux_test.go:35`
- Why the event is sufficient: the shim writes `exit` in `finishShim` (`shim.go:217-219`) before `runShim` returns (`shim.go:79`).
- Mutation probe: make `finishShim` return nil before it writes `exit`. `waitForExit` fails with "exited without an exit record".
- Spec: none.
- Anchor: Edsger W. Dijkstra.

#### Tests-and-docs step 8. `internal/runner/local_test.go:230, 253, 267, 430`, shim gate

- Causes:
  - `:230` sleeps 200 ms, assuming the shim has reached `start:5s` and that 5 s outlasts the signal.
  - `:253` polls for `ESRCH`, which waits for init to reap zombies.
  - `:267` polls `waitForFile`.
  - `:430` polls `waitForAlive`.
- Production change, `internal/runner/shim.go`:
  - `pauseShim` (`:144-152`): split `name, gate, ok := strings.Cut(...)`. When `name == phase`, `conn, err := net.Dial("unix", gate)`; on error return; otherwise `_, _ = io.Copy(io.Discard, conn)` and `_ = conn.Close()`. Add the `net` import.
  - In `startCommand`, after the `resumeCommand` check (`:118-121`) and before `return cmd, cleanup, nil`, add `pauseShim("group")`.
- Add `internal/runner/shimgate_test.go` (`//go:build !windows`). The gate directory is under `/tmp` so that the socket path stays under the 104-byte `sun_path` limit on macOS even where `TMPDIR` is long:
  - `type shimGate struct { dir, phase string; listener net.Listener; conn net.Conn; once sync.Once }`.
  - `newShimGate(t, phase)`: `os.MkdirTemp("/tmp", "gate")`, then `net.Listen("unix", filepath.Join(dir, "s"))`; registers `t.Cleanup(g.release)`.
  - `(g) env(base []string) []string`: falls back to `os.Environ()` when `base` is empty, clones it, and appends `shimPauseEnvironment + "=" + g.phase + ":" + g.listener.Addr().String()`.
  - `(g) await(t)`: `Accept`, storing `g.conn`; on error, `t.Fatalf("shim never reached phase %s: %v", g.phase, err)`.
  - `(g) release()`: inside `once`, close `conn` if set, close the listener, and `os.RemoveAll(dir)`.
  - `startGatedProcess(t, local, spec, phase) *shimGate`: sets `spec.Env = gate.env(spec.Env)`, calls `startTestProcess`, then registers `t.Cleanup(gate.release)` again so the gate opens before the process cleanup runs.
- Migrate the callers:
  - `TestLocal_rejectsMismatchedProcessIdentity` (`:143-178`): use the `start` gate and `await`, then run the existing body.
  - `TestLocal_signalPreservesExitRecord` (`:185-197`): use the `group` gate, `await`, `Signal`, `gate.release()`, `waitForExit`.
  - `TestLocal_signalBeforePIDPreventsStart` (`:199-221`): replace the `pid:300ms` env with the `pid` gate, `await` before `Signal`, `release` after.
  - `TestLocal_signalBeforeGroupReturnsNilAndEndsTerminated` (`:223-255`): use the `start` gate and `await`; delete `:229-230`; `release` after `Signal`. Replace the polling loop at `:245-254` with one check: `if !processGroupStopped(pgid) { t.Fatalf("process group %d outlived the signal", pgid) }`. The oracle is `runner.md`: "A group whose members are all zombies MUST count as stopped."
  - `internal/runner/local_pid_race_test.go`: use `startGatedProcess(..., "pid")` and `await`; delete the custom cleanup.
  - `internal/runner/local_start_cleanup_test.go:63-104`: use the `group` gate, `spec.Env = gate.env(nil)`, and `Argv` `{"sleep","30"}`; delete `started`. The hook calls `gate.await(t)`, `readProcessGroup`, `gate.release()`. Replace the loop at `:92-97` with one check: `if !processGroupStopped(target) { t.Fatalf("process group %d outlived the failed start", target) }`. `abortSpawn` removes the directory only after it confirms the stop (`local.go:132-134`).
- Delete `waitForFile` (`:257-269`) and `waitForAlive` (`:414-432`).
- Mutation probe: move `pauseShim("start")` after `cmd.Start()`. The `pgid existed before the signal` check fails.
- Spec: `docs/spec/runner.md`.
  - Old: "The shim MUST pause at startup only when `DALGONA_SHIM_TEST_PAUSE` is set to `<phase>:<duration>`. Phase `pid` pauses before the shim writes `pid`. Phase `start` pauses after the first `cancel` check and before the command starts. Only tests MAY set this variable."
  - New: "The shim MUST pause only when `DALGONA_SHIM_TEST_PAUSE` is set to `<phase>:<socket>`. At that phase it MUST connect to the Unix socket at `<socket>` and wait until the peer closes the connection. A failed connect or read MUST end the pause. Phase `pid` pauses before the shim writes `pid`. Phase `start` pauses after the first `cancel` check and before the command starts. Phase `group` pauses after the shim writes `pgid` and assigns the job object, and before the second `cancel` check. Only tests MAY set this variable."
- Anchor: Tony Hoare, David Parnas.

#### Tests-and-docs step 9. `internal/runner/process_group_linux_test.go:55`

- Cause: a 5 ms poll on a 10 s deadline waits for a group to look live.
- Replacement in `TestProcessGroupDrainedLiveGroup` (`:39-60`):
  1. `exec.Command("sh", "-c", "echo ready; exec sleep 30")` with `Setpgid`, and take `out, err := cmd.StdoutPipe()` before `Start`.
  2. Read with `bufio.NewReader(out).ReadString('\n')` and require exactly `"ready\n"`. The child has run inside its group by then.
  3. Assert `processGroupStopped(pgid)` is false with the message `processGroupStopped live group = true, want false`, then keep the `processGroupDrained` check.
- Delete the loop. Imports: add `bufio`; remove `time`, which only the deleted loop uses.
- Mutation probe: make `memberRunsGroup` (`process_group_linux.go:28-46`) return false. The test fails.
- Spec: none.
- Anchor: Richard Feynman.

#### Tests-and-docs step 10. `internal/runner/remote_atomic_write_test.go:41`

- Cause: a 500 ms injected write delay assumes the test's `cancel()` arrives while chunk 1 is still unanswered. Under load the upload can finish first, and the test reports "WriteFile succeeded after cancellation".
- Replacement:
  - In `scriptedFault` (`:19-26`), replace `writeDelay time.Duration` with `hold <-chan struct{}`. `stall` (`:33-46`) does `if f.hold != nil { <-f.hold }`.
  - Add `type uploadStall struct { firstWrite <-chan struct{}; release func() }`.
  - Replace `newStallRemote` and `newHungRemoveRemote` (`:92-104`) with `newStallRemote(t *testing.T, local *Local, dropRemove bool) (*Remote, uploadStall)`. It makes `hold`, wraps it with `release := sync.OnceFunc(func() { close(hold) })`, and registers `t.Cleanup(release)` after `serveFaultRemote`.
  - `cancelMidUpload(t *testing.T, remote *Remote, stall uploadStall, dest string, data []byte) error` (currently at `remote_atomic_write_test.go:260`) runs `<-stall.firstWrite`, `cancel()`, `stall.release()`, and keeps the 30 s guard, which bounds the contract's 5 s cleanup (`remote.go:270`).
  - Callers: `:285` becomes `newStallRemote(t, local, true)`; `:303` becomes `newStallRemote(t, local, false)`.
- Mutation probe: remove the `ctx.Done()` case in `Remote.wait` (`remote.go:467-477`; the case is at lines 469 to 471). `WriteFile` succeeds after the cancel.
- Spec: none.
- Anchor: Tony Hoare.

#### Tests-and-docs step 11. `internal/daemon/run_test.go:66`

- Cause: a 20 ms poll on a 10 s deadline, plus a 200 ms dial timeout and a 2 s read deadline in `greeting`.
- Production change, `internal/daemon/run.go`: add `var listenFunc = listen` and make `Run` call `listenFunc(home)` (`:40`).
- Test:
  - Add `observeListen(t) <-chan struct{}`. It wraps `listenFunc` so that a successful listen calls `sync.OnceFunc(func() { close(listening) })`, and restores `listenFunc` in `t.Cleanup`.
  - `startRun`: select on `<-listening` against `err := <-done`, which fails with `Run returned before it listened: %v`. Then require `greeting(home) == "ok"`.
  - `greeting`: use `net.Dial("unix", ...)` and `io.ReadAll` with no deadlines.
  - Delete `waitForGreeting`.
- Why the read finishes: `serveConn` closes the connection after `serve` returns (`run.go:117-122`).
- Mutation probe: make `listen` return before `net.Listen`. `startRun` fails at once.
- Spec: none.
- Anchor: David Parnas.

#### Tests-and-docs step 12. Hashline symbol, status and link drift

Scope and line numbers. This step edits only the lines listed below. The Specifications workstream owns every other line of `docs/spec`. In the Rust-era cargo block at `docs/spec/hashline/hashline-conformance-v1.md:162-172`, this step deletes only line 168 (listed below) and leaves the rest of the block as it is. Line numbers refer to the files before this step runs.

Tests first. In `internal/tool/patch/profile_test.go`, add `TestHashlineRejectsSymbolOperations`:
- Cases: style in `Hashline`, `HashlineLight`, `HashlineEnhanced`, crossed with op in `"PUT 2*:\n+x\n"`, `"CUT 2*\n"`, `"PUT >2*:\n+x\n"`.
- Inline the body in the table loop of `TestHashlineRejectsSymbolOperations`, in this order:
  1. `editTestEnv` and `writeFixture` with `"one\ntwo\nthree\n"`.
  2. `proof := styleProofFor(env, style, source)`.
  3. `runPatch(t, env, style, renderHashline("f.txt", nil, proof.ref)+op)` must return an error.
  4. The fixture must be unchanged.
- Probe: replace `hashline.go:293-295` with `text = strings.TrimSuffix(text, "*")`. The test fails.

`docs/spec/hashline/hashline-common-v1.md`:

| Line(s) | Old | New |
|---|---|---|
| 3 | "Status: reviewed proposal; not implemented or adopted as a product default." | "Status: implemented in `internal/tool/patch` as the `hashline-light` and `hashline-enhanced` edit styles; neither is a product default." |
| 10 | "MUST and MUST NOT define this proposed contract, not existing product behavior. Source-derived requirements and deliberate amendments are" | "MUST and MUST NOT define the contract that these two styles implement. Source-derived requirements and deliberate amendments are" |
| 110-111 | the rows `` `PUT N*:`, `CUT N*` `` and `` `PUT >N*:` `` | delete both rows |
| 255-268 | heading `## C09. AST operations` and its two paragraphs | see below |
| 472 | "`target_changed`, `gap_changed`, `node_changed`, `path_changed`" | "`target_changed`, `gap_changed`, `path_changed`" |
| 475 | the `symbols_unavailable` row | delete the row |

New text for lines 255-268:

> ## C09. Symbol operations
>
> Symbol locators are outside this contract. A profile MUST reject an operation whose locator contains `*`, such as `PUT N*:`, `CUT N*`, or `PUT >N*:`. The error MUST start with `patch: line <n>: block ops (N*) are not supported; use a line range`, and no target is written. Line ranges and gaps from read or search output address the same text.

`docs/spec/hashline/hashline-light-v1.md`:

| Line(s) | Old | New |
|---|---|---|
| 3 | "Status: reviewed proposal; not implemented or selected by default." | "Status: implemented in `internal/tool/patch` as edit style `hashline-light`; not selected by default." |
| 61 | "Syntax, mapping, gaps, N*, NEW, REM, MV, byte preservation, output, replay, and" | "Syntax, mapping, gaps, NEW, REM, MV, byte preservation, output, replay, and" |
| 79 | "The symbols-on description is:" | "The description is `lightDescription` in `internal/tool/patch/description.go`:" |
| 85 | "snapshot gaps. N* operations require enabled symbol support." | "snapshot gaps." |
| 97-98 | the "With symbols off..." two lines | delete them and the blank line before |

`docs/spec/hashline/hashline-enhanced-v1.md`:

| Line(s) | Old | New |
|---|---|---|
| 3 | "Status: reviewed proposal; not implemented or selected by default." | "Status: implemented in `internal/tool/patch` as edit style `hashline-enhanced`; not selected by default." |
| 82 | "The symbols-on description is:" | "The description is `enhancedDescription` in `internal/tool/patch/description.go`:" |
| 88 | "gaps. N* operations require enabled symbol support. Every existing cell" | "gaps. Every existing cell" |
| 101-102 | the "With symbols off..." two lines | delete them and the blank line before |

`docs/spec/hashline/README.md`:
- Lines 3-4. Old: "Reviewed proposal, 2026-09-28. These files specify two optional new profiles for / dal's existing patch engine. They do not implement the feature or change defaults." New: "Reviewed 2026-09-28 and adopted 2026-11-08. These files specify the `hashline-light` and `hashline-enhanced` edit styles that `internal/tool/patch` implements. Neither is a default."
- Lines 31-32. Old: the `package-checks.json` and `SHA256SUMS` lines, which link files that do not exist. New: delete both lines and the blank line after them.

`docs/spec/hashline/hashline-conformance-v1.md`:
- Delete rows `AST-01` to `AST-04` (`:60-63`).
- `:151`. Old: "| 4 | Patch: styles/hashline.rs, style.rs, ast.rs | New profiles, source-node resolution, section merge. |". New: "| 4 | Patch: styles/hashline.rs, style.rs | New profiles and section merge. |"
- Delete `:168`, the `--features symbols` cargo line.

Anchor: Edward Tufte.

#### Tests-and-docs step 13. Delete `approval_stale` from the spec

- Test: in `TestApprovalGapDriftRejectsPreparedPlan` (`internal/tool/patch/profile_test.go:358-373`), add `if err.Error() != "patch: f.txt changed while you were approving. Nothing was written. Try again" { t.Fatalf(...) }`. `file.path` is `group.display` (`internal/tool/patch/stage.go:112` and `:217`), which `resolveEnvPath` returns (`stage.go:125-133`). For input `f.txt`, `resolvePatchPath` (`stage.go:180-191`) returns `f.txt`, so the expected message is the one in the test.
- `hashline-common-v1.md`, the paragraph that begins "After approval, reacquire":
  - Old: "After approval, reacquire the full write set and recheck digests, revision/epoch, identity/path conditions, and absences. Any mismatch returns `approval_stale` without target writes."
  - New: "After approval, the writer rechecks every source digest and every required absence. A mismatch fails the call with `patch: <path> changed while you were approving. Nothing was written. Try again` and writes no target."
- Delete the table row whose first cell is `approval_stale`.
- Failing-first: none needed. The code already emits this text (`writer.go:90`), so the test pins the spec to it.
- Mutation probe: change one word in `writer.go:90`. The added test fails. Restore the text.
- Spec: `docs/spec/hashline/hashline-common-v1.md`, the sentence and row named above.
- Lands after: Step 12, which edits the same file and shifts its line numbers.
- Anchor: Tony Hoare.

#### Tests-and-docs step 14. `README.md` and `/login`

- Line 39, old: "In the terminal interface, run `/login <provider>` to sign in, then `/model` to select an offered model. Replace `<provider>` with your provider id, such as `openai` or `anthropic`. API-key providers can use their provider environment variables."
- Line 39, new: "In the terminal interface, run `/login <provider>` to open the sign-in panel for that provider, then `/model` to select an offered model. A ChatGPT account shows a browser link or a device code. An API-key provider such as `openai` or `anthropic` takes its key in the panel. A provider that signs in from a shell shows the command to run there. API-key providers can use their provider environment variables. No default thread model is shipped."
- Line 53: no change. The line already reads `/login <provider>` and `/model`, which stays true. No `dalgona login` command exists (`cmd/` has no `login` text).
- Source: the protocol workstream, Step 12 (the API-key path of `loginStart`) and the terminal workstream, Step 25 (the `/login` panel) define the shipped behaviour. Today's code is at `internal/rpc/serve_misc.go:215-241` and `internal/tui/commands.go:286-294`.
- Test: none. Reason: the text describes behaviour that the protocol workstream, Step 12 and the terminal workstream, Step 25 test.
- Spec: none. `README.md` is user documentation of shipped behaviour.
- Lands after: the protocol workstream, Step 12 and the terminal workstream, Step 25 (the `/login` panel). The protocol workstream, Step 12 does not edit `README.md`.
- Anchor: Italo Calvino.

#### Tests-and-docs step 15. Terms list

`docs/README.md`: insert after paragraph 2, before `## Specifications`:

> ## Terms
>
> Use one name for each thing in user-facing text and new specifications.
>
> - Thread: one conversation with its journal, turns, and settings. Write thread, not session. The storage names `session.created` and `sessions/` stay.
> - Follow-up: a prompt queued while a turn runs; it starts the next turn. `followup` is only the `user.message` kind.
> - Thinking and reasoning: interfaces show thinking; the wire, journal, and config field is `reasoning`.
> - Subagent: a thread that a tool call starts under another thread. `child.spawned` and `parent` are its journal names.
> - Interrupt: the user stops the running turn, which ends with status `interrupted`. `cancel` is only an approval decision.
> - Mode: the per-thread approval mode and sandbox mode, and the tool mode set in configuration. Name the remote shapes thread boxes and the daemon in a box.

`README.md:41`: old "to select an existing session"; new "to select an existing thread". This step changes only these words. The terminal workstream, Step 13 changes `Esc to interrupt a turn` on the same line.

Test: none. Reason: this is terms text. Each journal or config name it cites exists in code: `followup` (`internal/agent/event/event.go:205`), `child.spawned` (`internal/agent/event/event.go:26`), `parent` (`internal/agent/event/encode.go:55`), `interrupted` (`internal/agent/event/event.go:182`), `cancel` (`internal/agent/event/event.go:447`), `reasoning` (`internal/config/merge.go:25`), and `sessions/` (`internal/journal/journal.go:90`).
Spec: `docs/README.md`.

Anchor: Otl Aicher.

#### Tests-and-docs step 16. Decision rules and rejected alternatives

`docs/README.md`: after `## Decisions`, insert:

> Each decision file states its Invariant, Scope, and Enforcement. It MAY end with `## Rejected alternatives`; each entry names one alternative and the one reason it lost. A replaced decision MUST stay in this directory: add `Superseded by [<title>](<file>.md) on <YYYY-MM-DD>.` under its title, keep its text, and move its entry to the end of this list with the suffix `(superseded)`. Never delete a decision file.

Add this row to the list under `## Decisions`. Other steps add their own rows: `- [Edit styles](decisions/edit-styles.md): anchor default, explicit hashline profiles, and no symbol locators.`

Add to `docs/decisions/minimal-harness-sandbox.md`. This step owns the shared `## Rejected alternatives` text of this file. This step creates the heading with the two bullets. The protocol workstream, Step 13 lands later and keeps this file's invariant. The text is:

> ## Rejected alternatives
>
> - A container or virtual machine around every local command. It lost because this sandbox exists to stop accidental writes; isolating hostile code is outside this sandbox, as the [decision index](../README.md) says. [sandbox.md](../spec/sandbox.md) defines the write confinement.
> - Blocking network access by default. It lost because installing packages and fetching sources are normal agent work, and the policy guards writes.

Append to `docs/decisions/advisor.md`:

> ## Rejected alternatives
>
> - The advisor on by default. It lost because each review adds a model request per reviewed turn, and the user should choose that cost with `advisor.enabled = true` or `/advisor`.

New file `docs/decisions/edit-styles.md`:

> # Edit styles
>
> ## Invariant
>
> `anchor` MUST be the default edit style. `hashline-light` and `hashline-enhanced` MUST be selected only by the user's `edit_style` configuration; a model MUST NOT select a style. A refused Enhanced edit MUST NOT fall back to Light. The `patch` tool MUST reject symbol locators (`N*`).
>
> ## Scope
>
> This decision applies to `internal/tool/patch` and the `edit_style` key in `internal/config`. [edit-styles.md](../spec/edit-styles.md) defines grammars and refusals. [The hashline README](../spec/hashline/README.md) lists the profile contracts.
>
> ## Enforcement
>
> `TestHashlineRejectsSymbolOperations` in `internal/tool/patch/profile_test.go` checks rejection without a write. `just test` runs it.
>
> ## Rejected alternatives
>
> - `hashline-enhanced` as the default. It lost because C18 of [the shared contract](../spec/hashline/hashline-common-v1.md) forbids a default change until conformance, legacy regression tests and paired end-to-end edit evaluations run, and [the conformance suite](../spec/hashline/hashline-conformance-v1.md) states that its product checks were not executed in that task.
> - Symbol locators (`N*`). They lost because they need a language parser per file type, and line ranges from read output address the same text without one.

Test: step 12's `TestHashlineRejectsSymbolOperations`. This step adds no behaviour.
Spec: `docs/README.md` and the new `docs/decisions/edit-styles.md`.
- Lands after: Step 12, which adds `TestHashlineRejectsSymbolOperations`.

Anchor: Herbert Simon.

#### Tests-and-docs step 17. Process-group escape limit

Test: none. Reason: this documents a limit of the OS process model, not a code path. Spec: `docs/spec/runner.md`.

`docs/spec/runner.md`: in the same paragraph, directly after the sentence "It MUST NOT signal the shim.", insert one sentence. The core-fixes workstream, Step 8 adds its own sentence to another paragraph of this file:

> A descendant that calls `setsid` or `setpgid` leaves the command process group. Neither `Signal` nor the shim's group cleanup reaches it. This is a known limit, not a containment guarantee.

Anchor: Richard Feynman.

#### Tests-and-docs step 18. Mutation-probe rule

Test: none. Reason: contributor process text. Spec: none.

`CONTRIBUTE.md`: as a new paragraph after the paragraph that ends "...or repeat the implementation as the oracle.", append. The live-debug workstream, Step 3 adds its own line to this section; this step adds only this paragraph:

> Prove that a new test detects its defect with a mutation probe: put that defect into the code, for example by dropping, duplicating, or reordering one step, watch the test fail, then restore the code. Never commit the probe.

Anchor: Richard Feynman.

#### Tests-and-docs step 19. `DESIGN.md`, brand marks, icon font and licence notices

The file lands only with every value filled in. It must not ship naively empty and must contain no placeholder text. Its text is the section "Design record" of this plan, copied as written under the title `# Design`, with each heading one level higher than in the plan. The file must not refer to the plan. It keeps that record's sections in their order and gains the section that item 4 below names. Its required content, in this order:

1. Purpose: one visual system for `internal/tui` and `web/`; `AGENTS.md` routes interface changes here.
2. Values: the values that the "Design record" gives: the themes and colour roles, the one point, the thread hue, type, the chosen component candidates, motion and words, density, the status band, icons and the brand mark. This file is the only written source of these values. The web workstream, Step 8 copies the palette values into `web/src/lib/theme/palette.ts` and cites this file.
3. Accessibility gates: WCAG 2.2 AA contrast, 4.5:1 for text and 3:1 for UI, against the colour roles above; no state is shown by colour alone, so each state keeps its word; the focus ring above shows on every focusable control; 320 px reflow; live regions exist before their text changes; reduced motion, `DAL_NO_MOTION` and `NO_COLOR` stop motion and colour and leave the state words.
4. How to change this file: owner approval; the same commit updates `internal/tui` and `web/`; rejected options go to `docs/decisions/`.

Copy these files into the repository, unchanged and under their own names, in the same commit:
- The component values come from the section "Design record" only. No reference page is copied into the repository (cross-phase decision 31).
- `assets/brand/`: `dalgona.svg`, `dalgona-small.svg`, `dal.svg` and `dal-small.svg`. The text of each file is under "Brand mark sources" at the end of this step. Each file is that text and one final newline.
- `web/src/lib/theme/symbols-nerd-subset.woff2`: decode the base64 text under "Icon font subset" at the end of this step with `base64 -d`. The web workstream, Step 9 serves the font from this path. The font is the 27-glyph subset of Symbols Nerd Font Mono from Nerd Fonts 3.5.1. To build the same subset from the release instead, run this in a directory that holds `SymbolsNerdFontMono-Regular.ttf`: `uvx --from 'fonttools[woff]' pyftsubset SymbolsNerdFontMono-Regular.ttf --unicodes=U+E38D-E39B,U+F002A,U+F0238,U+F0256,U+F029A,U+F0341,U+F03D7,U+F051F,U+F0873-F0875,U+F0E47,U+F167A --flavor=woff2 --output-file=symbols-nerd-subset.woff2`. A rebuilt file can differ in bytes from the embedded one; then check its glyphs, not its checksum.
- `THIRD_PARTY_NOTICES.md`: create it with the heading `# Third-party notices` when it is absent, otherwise append. Add three sections in this order: `## Symbols Nerd Font Mono (MIT)`, `## Material Design Icons (Apache-2.0)`, and `## Weather Icons (SIL OFL 1.1)`. Each section gives the full licence text from the URL that the outside checks of this workstream name for it, and the copyright line quoted from that text. The Material Design Icons text is the Pictogrammers Free License. It gives Apache-2.0 for icons and fonts and MIT for code, and it prints no copyright line, so that section says so. The Weather Icons text prints only placeholder copyright lines, so the engineer stops and asks the owner for that section's copyright line before writing it.

Also add to the first paragraph of `docs/README.md`: "Use [DESIGN.md](../DESIGN.md) for the visual system."

Test: none. Reason: this step writes files and fixed text. Check each file with `sha256sum` against this table.

| File | Bytes | SHA-256 |
|---|---|---|
| `assets/brand/dalgona.svg` | 3792 | `00253e4d4a13ce855d64ad59246b0a25fc695740e544016c91507de8a098d1fe` |
| `assets/brand/dalgona-small.svg` | 2963 | `cf745f047d4a678e5472492c43f870f59daf5b6f892dc32ceec9dff751358630` |
| `assets/brand/dal.svg` | 3245 | `c7ba71edd5d1eb9fab322ebf2d7101161b6d582081e3d897b77cccb5cfbeadfd` |
| `assets/brand/dal-small.svg` | 3028 | `ac0cb851d9d9d5a8ed69413c6a7e08859eaed9ecd58d5950db820d6444de0c2d` |
| `web/src/lib/theme/symbols-nerd-subset.woff2` | 2792 | `90d49ec9c2e16e59c345fb86871b7843f4750c1f5d17c81ead511c3c854b9773` |
Spec: `DESIGN.md` holds the visual values. `THIRD_PARTY_NOTICES.md` holds the licences.

Anchor: Dieter Rams, Kenya Hara.

###### Brand mark sources

`assets/brand/dalgona.svg`:

```xml
<svg viewBox="0 0 120 120" xmlns="http://www.w3.org/2000/svg"><circle fill="#9D4310" cx="60" cy="66" r="50"/><circle fill="#DA702C" cx="60" cy="59" r="50"/><ellipse cx="66.25" cy="54.36" rx="1.66" ry="6.65" transform="rotate(-7 66.25 54.36)" stroke="#9D4310" fill="none" stroke-width="4.4"/><ellipse cx="74.29" cy="54.36" rx="1.66" ry="6.65" transform="rotate(7 74.29 54.36)" stroke="#9D4310" fill="none" stroke-width="4.4"/><ellipse cx="70.27" cy="64.62" rx="7.48" ry="6.58" transform="rotate(0 70.27 64.62)" stroke="#9D4310" fill="none" stroke-width="4.4"/><ellipse cx="66.25" cy="54.36" rx="1.66" ry="6.65" transform="rotate(-7 66.25 54.36)" fill="#DA702C"/><ellipse cx="74.29" cy="54.36" rx="1.66" ry="6.65" transform="rotate(7 74.29 54.36)" fill="#DA702C"/><ellipse cx="70.27" cy="64.62" rx="7.48" ry="6.58" transform="rotate(0 70.27 64.62)" fill="#DA702C"/><circle cx="67.29" cy="64.48" r="0.97" fill="#9D4310"/><circle cx="73.25" cy="64.48" r="0.97" fill="#9D4310"/><path d="M68.54 66.84Q70.27 68.78 72.00 66.84" fill="none" stroke-linecap="round" stroke-width="1.39" stroke="#9D4310"/><path d="M92.10 66.64L91.26 69.56L90.16 72.39L88.80 75.11L87.20 77.69L85.36 80.11L83.32 82.35L81.07 84.40L78.65 86.23L76.06 87.83L73.35 89.18L70.51 90.28L67.59 91.11L64.61 91.68L61.59 91.96L58.55 91.97L55.52 91.69L52.54 91.14L49.61 90.32L46.78 89.23L44.05 87.89L41.46 86.30L39.03 84.48L36.78 82.45L34.72 80.21L32.88 77.80L31.26 75.22L29.89 72.51L28.78 69.69L27.93 66.77L27.35 63.79L27.05 60.77L27.02 57.73L27.28 54.71L27.81 51.72L28.62 48.79L29.69 45.95L31.02 43.21L32.59 40.62L34.40 38.17L36.42 35.91L38.65 33.84L41.05 31.98L43.62 30.35L46.32 28.97L49.14 27.84L52.05 26.97L55.03 26.38L58.05 26.06L58.05 26.06L56.71 27.20L55.44 28.43L54.27 29.74L53.18 31.13L52.19 32.58L51.29 34.10L50.50 35.68L49.82 37.30L49.24 38.97L48.78 40.67L48.43 42.39L48.19 44.14L48.08 45.90L48.08 47.66L48.19 49.42L48.43 51.16L48.78 52.89L49.24 54.59L49.81 56.26L50.50 57.88L51.29 59.45L52.18 60.97L53.17 62.43L54.26 63.82L55.44 65.13L56.70 66.36L58.04 67.50L59.45 68.55L60.93 69.51L62.47 70.36L64.07 71.11L65.71 71.76L67.39 72.29L69.10 72.71L70.83 73.02L72.59 73.21L74.35 73.28L76.11 73.24L77.86 73.07L79.60 72.80L81.32 72.40L83.01 71.90L84.66 71.28L86.26 70.56L87.82 69.73L89.31 68.79L90.74 67.77L92.10 66.64Z" stroke-linejoin="round" fill="#DA702C"/><path d="M92.10 66.64L91.26 69.56L90.16 72.39L88.80 75.11L87.20 77.69L85.36 80.11L83.32 82.35L81.07 84.40L78.65 86.23L76.06 87.83L73.35 89.18L70.51 90.28L67.59 91.11L64.61 91.68L61.59 91.96L58.55 91.97L55.52 91.69L52.54 91.14L49.61 90.32L46.78 89.23L44.05 87.89L41.46 86.30L39.03 84.48L36.78 82.45L34.72 80.21L32.88 77.80L31.26 75.22L29.89 72.51L28.78 69.69L27.93 66.77L27.35 63.79L27.05 60.77L27.02 57.73L27.28 54.71L27.81 51.72L28.62 48.79L29.69 45.95L31.02 43.21L32.59 40.62L34.40 38.17L36.42 35.91L38.65 33.84L41.05 31.98L43.62 30.35L46.32 28.97L49.14 27.84L52.05 26.97L55.03 26.38L58.05 26.06L58.05 26.06L56.71 27.20L55.44 28.43L54.27 29.74L53.18 31.13L52.19 32.58L51.29 34.10L50.50 35.68L49.82 37.30L49.24 38.97L48.78 40.67L48.43 42.39L48.19 44.14L48.08 45.90L48.08 47.66L48.19 49.42L48.43 51.16L48.78 52.89L49.24 54.59L49.81 56.26L50.50 57.88L51.29 59.45L52.18 60.97L53.17 62.43L54.26 63.82L55.44 65.13L56.70 66.36L58.04 67.50L59.45 68.55L60.93 69.51L62.47 70.36L64.07 71.11L65.71 71.76L67.39 72.29L69.10 72.71L70.83 73.02L72.59 73.21L74.35 73.28L76.11 73.24L77.86 73.07L79.60 72.80L81.32 72.40L83.01 71.90L84.66 71.28L86.26 70.56L87.82 69.73L89.31 68.79L90.74 67.77L92.10 66.64Z" stroke-linejoin="round" stroke="#9D4310" fill="none" stroke-width="4.0"/><path stroke="#FFFCF0" stroke-opacity=".45" d="M17.9 50.1A43 43 0 0 1 40.5 20.7" fill="none" stroke-width="5" stroke-linecap="round"/><circle fill="#FFFCF0" fill-opacity=".45" cx="49.6" cy="17.3" r="3"/></svg>
```

`assets/brand/dalgona-small.svg`:

```xml
<svg viewBox="0 0 120 120" xmlns="http://www.w3.org/2000/svg"><circle fill="#9D4310" cx="60" cy="66" r="54"/><circle fill="#DA702C" cx="60" cy="56" r="54"/><ellipse cx="64.13" cy="42.35" rx="3.37" ry="10.78" transform="rotate(-10 64.13 42.35)" fill="#9D4310"/><ellipse cx="78.05" cy="42.35" rx="3.37" ry="10.78" transform="rotate(10 78.05 42.35)" fill="#9D4310"/><ellipse cx="71.09" cy="58.51" rx="12.12" ry="10.67" transform="rotate(0 71.09 58.51)" fill="#9D4310"/><path d="M94.67 64.26L93.76 67.41L92.57 70.46L91.11 73.40L89.37 76.18L87.39 78.80L85.18 81.22L82.76 83.43L80.14 85.41L77.35 87.13L74.41 88.60L71.36 89.78L68.20 90.68L64.98 91.29L61.71 91.60L58.43 91.61L55.16 91.31L51.94 90.72L48.78 89.83L45.72 88.65L42.78 87.20L39.98 85.49L37.35 83.52L34.92 81.32L32.70 78.91L30.71 76.30L28.97 73.52L27.49 70.59L26.28 67.54L25.36 64.39L24.74 61.17L24.41 57.91L24.39 54.63L24.66 51.36L25.24 48.13L26.11 44.97L27.27 41.90L28.70 38.95L30.40 36.15L32.35 33.51L34.54 31.06L36.94 28.83L39.54 26.82L42.30 25.06L45.22 23.57L48.27 22.35L51.41 21.41L54.63 20.77L57.89 20.42L57.89 20.42L56.44 21.66L55.08 22.99L53.81 24.40L52.63 25.90L51.56 27.47L50.60 29.11L49.74 30.81L49.00 32.56L48.38 34.36L47.88 36.20L47.50 38.06L47.25 39.95L47.12 41.85L47.12 43.75L47.25 45.65L47.50 47.54L47.88 49.40L48.38 51.24L49.00 53.04L49.74 54.79L50.59 56.49L51.56 58.13L52.63 59.70L53.80 61.20L55.07 62.62L56.44 63.95L57.88 65.18L59.41 66.32L61.01 67.35L62.67 68.27L64.39 69.08L66.16 69.78L67.98 70.35L69.83 70.81L71.70 71.14L73.59 71.34L75.49 71.42L77.40 71.37L79.29 71.20L81.17 70.90L83.03 70.48L84.85 69.93L86.63 69.26L88.37 68.48L90.04 67.58L91.66 66.58L93.20 65.47L94.67 64.26Z" stroke-linejoin="round" fill="#DA702C" stroke="#DA702C" stroke-width="5"/><path d="M94.67 64.26L93.76 67.41L92.57 70.46L91.11 73.40L89.37 76.18L87.39 78.80L85.18 81.22L82.76 83.43L80.14 85.41L77.35 87.13L74.41 88.60L71.36 89.78L68.20 90.68L64.98 91.29L61.71 91.60L58.43 91.61L55.16 91.31L51.94 90.72L48.78 89.83L45.72 88.65L42.78 87.20L39.98 85.49L37.35 83.52L34.92 81.32L32.70 78.91L30.71 76.30L28.97 73.52L27.49 70.59L26.28 67.54L25.36 64.39L24.74 61.17L24.41 57.91L24.39 54.63L24.66 51.36L25.24 48.13L26.11 44.97L27.27 41.90L28.70 38.95L30.40 36.15L32.35 33.51L34.54 31.06L36.94 28.83L39.54 26.82L42.30 25.06L45.22 23.57L48.27 22.35L51.41 21.41L54.63 20.77L57.89 20.42L57.89 20.42L56.44 21.66L55.08 22.99L53.81 24.40L52.63 25.90L51.56 27.47L50.60 29.11L49.74 30.81L49.00 32.56L48.38 34.36L47.88 36.20L47.50 38.06L47.25 39.95L47.12 41.85L47.12 43.75L47.25 45.65L47.50 47.54L47.88 49.40L48.38 51.24L49.00 53.04L49.74 54.79L50.59 56.49L51.56 58.13L52.63 59.70L53.80 61.20L55.07 62.62L56.44 63.95L57.88 65.18L59.41 66.32L61.01 67.35L62.67 68.27L64.39 69.08L66.16 69.78L67.98 70.35L69.83 70.81L71.70 71.14L73.59 71.34L75.49 71.42L77.40 71.37L79.29 71.20L81.17 70.90L83.03 70.48L84.85 69.93L86.63 69.26L88.37 68.48L90.04 67.58L91.66 66.58L93.20 65.47L94.67 64.26Z" stroke-linejoin="round" fill="#9D4310"/></svg>
```

`assets/brand/dal.svg`:

```xml
<svg viewBox="0 0 120 120" xmlns="http://www.w3.org/2000/svg"><mask id="dal-mask"><rect width="120" height="120" fill="#000"/><ellipse cx="69.85" cy="52.69" rx="2.62" ry="10.48" transform="rotate(-7 69.85 52.69)" fill="#fff"/><ellipse cx="82.52" cy="52.69" rx="2.62" ry="10.48" transform="rotate(7 82.52 52.69)" fill="#fff"/><ellipse cx="76.18" cy="68.85" rx="11.79" ry="10.37" transform="rotate(0 76.18 68.85)" fill="#fff"/><circle cx="71.49" cy="68.63" r="1.53" fill="#000"/><circle cx="80.88" cy="68.63" r="1.53" fill="#000"/><path d="M73.45 72.35Q76.18 75.41 78.91 72.35" fill="none" stroke-linecap="round" stroke-width="2.18" stroke="#000"/><path d="M110.59 72.04L109.26 76.64L107.53 81.10L105.38 85.38L102.86 89.45L99.97 93.26L96.74 96.80L93.20 100.02L89.38 102.90L85.31 105.42L81.03 107.56L76.57 109.29L71.97 110.60L67.26 111.49L62.50 111.94L57.71 111.95L52.94 111.52L48.24 110.65L43.63 109.36L39.16 107.64L34.87 105.52L30.79 103.02L26.96 100.15L23.41 96.95L20.17 93.42L17.26 89.62L14.72 85.57L12.56 81.29L10.80 76.84L9.46 72.25L8.55 67.55L8.07 62.79L8.04 58.00L8.44 53.23L9.28 48.52L10.55 43.91L12.24 39.43L14.34 35.12L16.82 31.03L19.66 27.18L22.85 23.61L26.35 20.35L30.14 17.43L34.18 14.86L38.44 12.68L42.88 10.90L47.47 9.53L52.16 8.59L56.92 8.09L56.92 8.09L54.81 9.89L52.82 11.83L50.97 13.90L49.25 16.08L47.69 18.37L46.28 20.77L45.03 23.25L43.95 25.81L43.05 28.43L42.32 31.11L41.77 33.83L41.40 36.58L41.21 39.35L41.21 42.13L41.40 44.90L41.76 47.65L42.31 50.37L43.04 53.05L43.95 55.68L45.03 58.23L46.27 60.72L47.68 63.11L49.24 65.40L50.96 67.59L52.81 69.65L54.80 71.59L56.91 73.39L59.14 75.05L61.47 76.56L63.90 77.91L66.41 79.09L68.99 80.10L71.64 80.94L74.34 81.61L77.07 82.09L79.83 82.39L82.61 82.50L85.38 82.43L88.15 82.18L90.89 81.74L93.60 81.12L96.25 80.33L98.86 79.35L101.39 78.21L103.83 76.90L106.19 75.43L108.44 73.81L110.59 72.04Z" stroke-linejoin="round" fill="#000" stroke="#000" stroke-width="7"/><path d="M110.59 72.04L109.26 76.64L107.53 81.10L105.38 85.38L102.86 89.45L99.97 93.26L96.74 96.80L93.20 100.02L89.38 102.90L85.31 105.42L81.03 107.56L76.57 109.29L71.97 110.60L67.26 111.49L62.50 111.94L57.71 111.95L52.94 111.52L48.24 110.65L43.63 109.36L39.16 107.64L34.87 105.52L30.79 103.02L26.96 100.15L23.41 96.95L20.17 93.42L17.26 89.62L14.72 85.57L12.56 81.29L10.80 76.84L9.46 72.25L8.55 67.55L8.07 62.79L8.04 58.00L8.44 53.23L9.28 48.52L10.55 43.91L12.24 39.43L14.34 35.12L16.82 31.03L19.66 27.18L22.85 23.61L26.35 20.35L30.14 17.43L34.18 14.86L38.44 12.68L42.88 10.90L47.47 9.53L52.16 8.59L56.92 8.09L56.92 8.09L54.81 9.89L52.82 11.83L50.97 13.90L49.25 16.08L47.69 18.37L46.28 20.77L45.03 23.25L43.95 25.81L43.05 28.43L42.32 31.11L41.77 33.83L41.40 36.58L41.21 39.35L41.21 42.13L41.40 44.90L41.76 47.65L42.31 50.37L43.04 53.05L43.95 55.68L45.03 58.23L46.27 60.72L47.68 63.11L49.24 65.40L50.96 67.59L52.81 69.65L54.80 71.59L56.91 73.39L59.14 75.05L61.47 76.56L63.90 77.91L66.41 79.09L68.99 80.10L71.64 80.94L74.34 81.61L77.07 82.09L79.83 82.39L82.61 82.50L85.38 82.43L88.15 82.18L90.89 81.74L93.60 81.12L96.25 80.33L98.86 79.35L101.39 78.21L103.83 76.90L106.19 75.43L108.44 73.81L110.59 72.04Z" stroke-linejoin="round" fill="#fff"/></mask><rect fill="currentColor" width="120" height="120" mask="url(#dal-mask)"/></svg>
```

`assets/brand/dal-small.svg`:

```xml
<svg viewBox="0 0 120 120" xmlns="http://www.w3.org/2000/svg"><mask id="dal-small-mask"><rect width="120" height="120" fill="#000"/><ellipse cx="66.49" cy="38.55" rx="5.29" ry="16.93" transform="rotate(-10 66.49 38.55)" fill="#fff"/><ellipse cx="88.37" cy="38.55" rx="5.29" ry="16.93" transform="rotate(10 88.37 38.55)" fill="#fff"/><ellipse cx="77.43" cy="63.95" rx="19.05" ry="16.76" transform="rotate(0 77.43 63.95)" fill="#fff"/><path d="M114.48 72.97L113.05 77.92L111.18 82.73L108.87 87.34L106.15 91.71L103.04 95.82L99.57 99.63L95.76 103.10L91.64 106.20L87.26 108.92L82.65 111.22L77.84 113.08L72.89 114.50L67.82 115.45L62.69 115.94L57.54 115.95L52.40 115.48L47.33 114.55L42.37 113.15L37.56 111.31L32.94 109.03L28.54 106.33L24.42 103.24L20.59 99.79L17.10 96.00L13.97 91.90L11.24 87.53L8.91 82.93L7.02 78.14L5.58 73.19L4.59 68.13L4.08 63.00L4.04 57.85L4.48 52.71L5.38 47.64L6.75 42.67L8.57 37.85L10.82 33.21L13.49 28.80L16.56 24.66L19.99 20.82L23.76 17.30L27.84 14.15L32.20 11.39L36.78 9.04L41.57 7.12L46.51 5.65L51.56 4.64L56.69 4.10L56.69 4.10L54.41 6.04L52.27 8.13L50.27 10.35L48.43 12.70L46.74 15.17L45.22 17.75L43.88 20.42L42.72 23.18L41.74 26.00L40.96 28.89L40.36 31.82L39.97 34.78L39.77 37.76L39.77 40.75L39.96 43.74L40.36 46.70L40.95 49.63L41.74 52.52L42.71 55.34L43.87 58.10L45.22 60.77L46.73 63.35L48.42 65.82L50.26 68.17L52.26 70.40L54.40 72.48L56.67 74.42L59.07 76.21L61.58 77.83L64.20 79.28L66.90 80.56L69.69 81.65L72.54 82.56L75.44 83.27L78.38 83.79L81.36 84.11L84.35 84.23L87.33 84.16L90.31 83.88L93.26 83.41L96.18 82.75L99.04 81.89L101.84 80.84L104.57 79.61L107.21 78.20L109.74 76.62L112.17 74.87L114.48 72.97Z" stroke-linejoin="round" fill="#000" stroke="#000" stroke-width="8"/><path d="M114.48 72.97L113.05 77.92L111.18 82.73L108.87 87.34L106.15 91.71L103.04 95.82L99.57 99.63L95.76 103.10L91.64 106.20L87.26 108.92L82.65 111.22L77.84 113.08L72.89 114.50L67.82 115.45L62.69 115.94L57.54 115.95L52.40 115.48L47.33 114.55L42.37 113.15L37.56 111.31L32.94 109.03L28.54 106.33L24.42 103.24L20.59 99.79L17.10 96.00L13.97 91.90L11.24 87.53L8.91 82.93L7.02 78.14L5.58 73.19L4.59 68.13L4.08 63.00L4.04 57.85L4.48 52.71L5.38 47.64L6.75 42.67L8.57 37.85L10.82 33.21L13.49 28.80L16.56 24.66L19.99 20.82L23.76 17.30L27.84 14.15L32.20 11.39L36.78 9.04L41.57 7.12L46.51 5.65L51.56 4.64L56.69 4.10L56.69 4.10L54.41 6.04L52.27 8.13L50.27 10.35L48.43 12.70L46.74 15.17L45.22 17.75L43.88 20.42L42.72 23.18L41.74 26.00L40.96 28.89L40.36 31.82L39.97 34.78L39.77 37.76L39.77 40.75L39.96 43.74L40.36 46.70L40.95 49.63L41.74 52.52L42.71 55.34L43.87 58.10L45.22 60.77L46.73 63.35L48.42 65.82L50.26 68.17L52.26 70.40L54.40 72.48L56.67 74.42L59.07 76.21L61.58 77.83L64.20 79.28L66.90 80.56L69.69 81.65L72.54 82.56L75.44 83.27L78.38 83.79L81.36 84.11L84.35 84.23L87.33 84.16L90.31 83.88L93.26 83.41L96.18 82.75L99.04 81.89L101.84 80.84L104.57 79.61L107.21 78.20L109.74 76.62L112.17 74.87L114.48 72.97Z" stroke-linejoin="round" fill="#fff"/></mask><rect fill="currentColor" width="120" height="120" mask="url(#dal-small-mask)"/></svg>
```

###### Icon font subset

`web/src/lib/theme/symbols-nerd-subset.woff2`, base64:

```
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
```

#### Tests-and-docs step 20. `AGENTS.md`

The full new content:

```markdown
# Agent instructions for [dal and dalgona](README.md)

## Core philosophy

Be opinionated. Do not add every piece of junk in the world. Do not ship something naively empty.

- Pick one answer per decision and add no option to avoid the choice. [Steve Jobs]
- Name the user task before adding a feature, command or layer. [Steve Jobs]
- Never remove a control, label, state, check or error path the user needs and call it minimal. [Steve Jobs]
- A feature works from start to finish with its defaults before the next one is added. [Steve Jobs]

## Design

- Each visible element serves the task. [Dieter Rams]
- Leave space empty and never hide what the user needs to act. [Kenya Hara]
- Interface changes follow [DESIGN.md](DESIGN.md). [Dieter Rams, Kenya Hara]

## Contracts

- You MUST run `just verify` from [justfile](justfile) before every commit. [Steve Jobs]
- You MUST keep the package boundaries in [.golangci.yml](.golangci.yml). A new import across a boundary MUST have a decision in [docs/decisions](docs/README.md#decisions). [Steve Jobs]
- Each behavior change MUST update its topic in [docs/spec](docs/README.md#specifications) in the same commit. [Steve Jobs]
- You MUST keep the journal append-only under [journal.md](docs/spec/journal.md). Recovery MUST follow [recovery.md](docs/spec/recovery.md). [Steve Jobs]
- Hashline profile changes MUST follow [the shared contract](docs/spec/hashline/hashline-common-v1.md), [Light](docs/spec/hashline/hashline-light-v1.md), [Enhanced](docs/spec/hashline/hashline-enhanced-v1.md), and [conformance](docs/spec/hashline/hashline-conformance-v1.md). [Steve Jobs]
- You MUST keep one concern per commit for the module in [go.mod](go.mod). You MUST use the global git identity. You MUST NOT add agent trailers. [Steve Jobs]
- You MUST apply the contribution contract in [CONTRIBUTE.md](CONTRIBUTE.md) and the review contract in [REVIEW.md](REVIEW.md). [Steve Jobs]
```

Test: none. Reason: this is instruction text. Every link target it names must exist after step 19: `README.md`, `justfile`, `.golangci.yml`, `go.mod`, `CONTRIBUTE.md`, `REVIEW.md`, `docs/README.md`, `docs/spec/journal.md`, `docs/spec/recovery.md`, `docs/spec/hashline/`, and `DESIGN.md`.
Spec: `AGENTS.md`, replaced in full by the content above.
- Lands after: Step 19 (the `DESIGN.md` link must resolve).

Anchor: Dieter Rams.

##### Site map (acceptance)

| # | Site | Step |
|---|---|---|
| 1 | `request_models_test.go:173` | 2 |
| 2 | `childcap_test.go:76` | 3 |
| 3 | `misc_test.go:309` | 4 |
| 4 | `steer_atomic_test.go:82` | 5 |
| 5 | `override_test.go:190` | 6 |
| 6 | `local_test.go:410` | 7 |
| 7 | `local_test.go:301` | 7 |
| 8 | `local_test.go:230` | 8 |
| 9 | `local_test.go:253` | 8 |
| 10 | `local_test.go:267` | 8 |
| 11 | `local_test.go:430` | 8 |
| 12 | `process_group_linux_test.go:55` | 9 |
| 13 | `remote_atomic_write_test.go:41` | 10 |
| 14 | `daemon/run_test.go:66` | 11 |

#### Schemas of the tests-and-docs workstream

- Environment variable `DALGONA_SHIM_TEST_PAUSE` (test only):
  - Value `<phase>:<socket path>`.
  - `phase` is one of `pid`, `start`, `group`.
  - `socket path` is an absolute Unix socket path of fewer than 104 bytes.
  - Example: `DALGONA_SHIM_TEST_PAUSE=group:/tmp/gate123/s`.
- Package variable `listenFunc func(string) (net.Listener, error)` in `internal/daemon`. It is a test seam with no user surface.
- This workstream adds no wire message, journal event, config key or tool argument.

#### Checks of the tests-and-docs workstream

- Per step: `go test -race -count=1 ./internal/agent/` (steps 1 to 6), `./internal/runner/` (7 to 10), `./internal/daemon/` (11), `./internal/tool/patch/` (12 and 13). Run each listed mutation probe, see the test fail, then restore the file.
- Under load:
  1. Start `go test -race -count=3 ./... &` as background load.
  2. Run `go test -race -count=40 -cpu 1,2,8 -run 'TestProviderModelsCollapsesConcurrentLookups|TestStartChildBoundsConcurrentSpawns|TestConsecutiveAgentCallsRunTogether|TestRejectedSteerSkipsWorkspaceTrust|TestToolSetBuildDoesNotBlockSubmit' ./internal/agent/`.
  3. Run `go test -race -count=20 -run 'TestLocal_|TestLocalReadAfterStartReportsLive|TestProcessGroupDrained|TestRemoteWriteFile' ./internal/runner/`.
  4. Run `go test -race -count=20 -run TestRun ./internal/daemon/`.
  5. Expect zero failures.
- End to end: `just verify`.
- Call-site check: `grep -rn 'waitForExit(' internal/runner` lists only the call sites in step 7, each without a timeout argument.
- Asset check: `sha256sum` of the four files in `assets/brand/` and of `web/src/lib/theme/symbols-nerd-subset.woff2` equals the table in Step 19.

#### Prerequisites of the tests-and-docs workstream

- No test step depends on another workstream. Step 7 uses `unix.PidfdOpen`, `unix.Waitid` and `unix.P_PIDFD`. The pinned `golang.org/x/sys v0.49.0` already provides them (`unix/zsyscall_linux.go`, `unix/zerrors_linux.go` in the module cache), so Step 7 does not wait for the pins.
- Step 14 lands after the protocol workstream, Step 12 and the terminal workstream, Step 25, so `README.md:39` describes the `/login` panel that ships. The protocol workstream, Step 12 changes `protocol.md` and `docs/spec/oauth.md` only. The terminal workstream, Step 13 changes `README.md:41`; Step 15 changes another phrase on that line.
- Step 19 takes its text from the section "Design record" of this plan and its font path from the web workstream, Step 9. It needs no other input.
- Step 20 lands after step 19 so that its `DESIGN.md` link resolves.
- Steps 12 and 13 both edit `docs/spec/hashline/hashline-common-v1.md`; Step 13 lands after Step 12. Step 16 shares `docs/decisions/minimal-harness-sandbox.md` with the protocol workstream, Step 13 (see Step 16).

