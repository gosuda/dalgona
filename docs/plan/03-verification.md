## Verification

Run these checks. Each names what proves it.

1. The gate. `just verify` passes after every step and at the last commit (`justfile:121`: format, vet, lint, race tests, vulnerability check, the hashline model, the specification style check after Specifications step 40, the web checks and the Cloudflare checks).
2. A test that fails first. For each step that names a failing-first test, run the test before the change and see it fail for the reason the step gives. Then make the change and see it pass. A test that did not fail first proves nothing: fix the test before you go on.
3. Deterministic tests. For each test that Tests-and-docs steps 1 to 11 change, run `go test -race -count=20 -run '<name>' <package>` under load (`stress-ng --cpu 0` or a parallel `go build ./...` loop). Every run passes.
4. Specifications. `just spec-style` prints `style findings: 0`. Each Specifications step and each Spec-fix step gives its own proof.
5. The terminal screen. The invariant tests of the section "Terminal screen design" pass: `TestDockBoundsOverScriptedTurn` (I6), `TestDockHeightChangesOnlyOnKeys` (I2), `TestLiveRowFieldsKeepCells` (I3), and the tests that the terminal workstream names for I1, I4, I5, I7 and I8. The byte-stream tests run the program with `tea.WithOutput` and assert that the output holds no erase-display sequence and no cursor-home sequence.
6. The browser. `just web-check` passes (Biome, Vitest with the component tests, `svelte-check`). `just web` builds.
7. The checks of each workstream. Each workstream ends with a section "Checks of the ... workstream" that lists its commands and manual checks. Run them when the workstream's last step lands.
8. The live sessions. Live-debug step 4 runs last. It starts both products against the scripted model server with `just live-up`, drives the terminal through the `tui` tool for scenarios T01 to T48, drives the browser for rows W01 to W30, including W21b, and runs W24 as its named gate, and writes the report that the step names. A scenario passes only when its exact observation is seen. Each failure goes through the defect loop of that step: reproduce, write a failing test, fix, run the scenario again.
9. The end state. Every command and integration in `README.md` and `docs/spec` works as written. Check each row of the site map in the tests-and-docs workstream, and each method row of `docs/spec/protocol.md`, against a live run or a named test.
10. The code tools. With the index built for a workspace, `search` in `grep` mode returns the same bytes as with the index removed (the property test of the code-search workstream and one manual call on this repository). `search` with `mode` `symbols` and `path` `internal/script/mode.go` lists each top-level constant, variable and function of that file once with its line range, and no local variable. Read the file at that commit and compare the names by eye. A cell in a fresh `dal` and a fresh `dalgona` calls `tools.read` and `tools.todo` with no configuration.
11. No prose comment. `go test ./internal/commentgate/...` passes, and a prose comment added to any Go, Svelte, Python or hand-written TypeScript file makes it fail with the path and line.
12. Windows compile. `GOOS=windows go vet ./internal/tool/patch/...` passes. It proves that every `os.Root` method that the patch code calls exists on the pinned toolchain (Core-fixes decision 10).

