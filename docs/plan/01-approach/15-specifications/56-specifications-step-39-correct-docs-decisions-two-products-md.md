#### Specifications step 39. Correct `docs/decisions/two-products.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/decisions/two-products.md` in one commit. Line numbers are from commit `3c4b6f94`.

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 7 | A relative `$DAL_HOME` or `$DALGONA_HOME` is made absolute by `config.AnchorHome`, and `Load` rejects a relative home. If the user home directory is unknown, the home is the relative `.dal` or `.dalgona` directory. | internal/config/config.go:23-35 Product.Home uses the environment value, then os.UserHomeDir with the dir name, and returns dir at lines 32-33 when the home is unknown; config.go:38-41 AnchorHome makes a non-empty relati |

Structure:

- Homes: config.go:23-35 uses DALGONA_HOME or DAL_HOME, then the user home with .dalgona or .dal.
- Dalgona-only keys: merge.go:530 dalgonaOnlyKeys; merge.go:548 returns '%s:%d: %s is a dalgona setting'; config_test.go:235-237 pins the text for dal.
- Battery wiring: cmd/dalgona/daemon.go:57-58 passes Batteries: batteries.Load for dalgona; cmd/dal/main.go:52 uses config.ProductDal.Home().
- Depguard: .golangci.yml rule 'dal' denies daemon, script, sandboxsvc, mcp, skills, discovery, webui, herdr and codexapp to cmd/dal.
- Module: justfile:1-2 builds ./cmd/... in one go build. go.mod:1 declares module `github.com/gosuda/dalgona`, and go.mod:3 sets `go 1.27.0`.

Do not rewrite the rest of this file. Proof: `just verify` passes.

