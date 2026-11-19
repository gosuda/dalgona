#### Spec-fix step 5. Ignore instruction files that resolve outside their root
Anchor: Tony Hoare, Richard Feynman

The text is right and the code breaks it. `docs/spec/discovery.md` line 41 says: "The loader MUST resolve each candidate and MUST ignore a file that resolves outside its root." Keep that sentence.

- Where: `internal/discovery/discovery.go:tree`, `internal/discovery/discovery.go:readInstruction`.
- Change: In tree, resolve each candidate with filepath.EvalSymlinks and resolve its owning root the same way: the home root for the home candidate, the cwd root for the cwd candidate. If the candidate does not exist, keep the current nil result. If the resolved candidate is not inside the resolved root (filepath.Rel result starts with ..), return no instruction for that candidate and no error. Read the candidate only after this check. readInstruction keeps its os.ReadFile call.
- Failing-first test: `TestInstructionSymlinkOutsideRootIsIgnored` in `internal/discovery/discovery_test.go`. Input: home is a temp dir whose .codex/AGENTS.md is a symbolic link to <tmp>/secret/AGENTS.md, and that file holds the text secret; cwd is an empty temp dir; call discovery.Discover(home, cwd, []string{"codex"}, discovery.Native{}) Expected: no returned instruction has Path equal to <home>/.codex/AGENTS.md, and no instruction text equals secret The expected value comes from: Spec line 41 says a file that resolves outside its root MUST be ignored. The expected value is an absence, so no value from the code is copied into the test.
- Evidence at commit `3c4b6f94`: internal/discovery/discovery.go:180 `item, err := readInstruction(filepath.Join(root, instruction))`; internal/discovery/discovery.go:197 `data, err := os.ReadFile(path)`; grep for EvalSymlinks in internal/discovery: no match
- Proof: the test fails before the change and passes after it, and `just verify` passes.

