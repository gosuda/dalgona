#### Protocol step 19. `fuzzyFileSearch`

- `internal/tool/search/engine.go`: `func Files(root string, limit int) ([]string, error)` by calling `walkScope(root, root)` (line 102) and returning each entry's `display` field (slash-separated, relative to `root`), at most `limit` paths in sorted order. `fuzzyFileSearch` passes 100000, the walk cap of the fuzzy rule. The server keeps each root's list for 5 seconds, because clients send a request on every keystroke.
- New `docs/decisions/rpc-file-search.md` for the import `internal/rpc` to `internal/tool/search` (invariant, scope, enforcement, "Rejected alternatives": a second walker).
- New `internal/rpc/serve_fuzzy.go`: `(*server).fuzzyFileSearch`; `func fuzzyScore(query, path string) (uint32, []uint32, bool)` with the rule in Schemas; per-connection `searches map[string]context.CancelFunc` keyed by `cancellationToken`.
- `responseSchemas["fuzzyFileSearch"] = "FuzzyFileSearchResponse.json"`.
- Test `internal/rpc` `TestFuzzyFileSearchRanksAndHonoursGitignore`: files `docs/serve.md`, `internal/rpc/serve.go`, `ignored/serve.go`, `.gitignore` = `ignored/`; query `serve` returns `docs/serve.md` then `internal/rpc/serve.go` (shorter path wins), never the ignored file. Fails before: `-32601`.
- Docs: `protocol.md`, `docs/spec/search.md`.

Anchor: Claude Shannon, Herbert Simon.

