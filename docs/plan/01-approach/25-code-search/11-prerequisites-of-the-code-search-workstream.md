#### Prerequisites of the code-search workstream

- Specifications step 31 (the rewrite of `docs/spec/search.md`) lands before Code search step 3. It cites `engine.go` lines at `3c4b6f94`, and this workstream moves those lines.
- The Version pins workstream lands before Code search step 3, because its table in `docs/decisions/dependencies.md` holds the codesearch row that step 3 edits.
- Specifications step 40 adds `just spec-style`. It lands before Code search step 3, so that every sentence of steps 3 to 6 is checked.
- The sandbox tests need Linux with Landlock or macOS with `/usr/bin/sandbox-exec`. They skip elsewhere. The other tests run on any Unix host.
- Cross-phase decision 25 (Version pins) owns the other `go.mod` versions. This workstream changes only the codesearch require line.

