### Workstream: Version pins

Nine pins rise: the Go toolchain, six direct Go modules, and two web packages. Every other pin stays. Three direct modules are new and raise no pin: github.com/google/codesearch v1.2.0, github.com/odvcencio/gotreesitter v0.57.0, and the test-only github.com/charmbracelet/x/vt v0.1.0. The indirect module `ultraviolet` moves only if `go mod tidy` selects a newer version.

#### Version pins on 2026-11-11

| Entry | Pin in repo | Latest stable | Released | Advisory at pin | Decision |
|---|---|---|---|---|---|
| Go toolchain | go1.27.3 (go.mod:5); go 1.27.0 (go.mod:3) | go1.27.4 | 2026-11-10 | GO-2026-6859, 6848, 6850, 6858 open at 1.27.3; fixed in 1.27.4 | Raise to go1.27.4 by hand edit (Step 1) |
| charm.land/bubbletea/v2 | v2.1.1 (go.mod:14) | v2.1.1 | 2026-10-28 | GHSA-qc58-pfg9-7pph fixed in v2.1.1 | Keep |
| charm.land/lipgloss/v2 | v2.0.6 (go.mod:16) | v2.1.0 | 2026-11-09 | none | Raise to v2.1.0 (Step 3) |
| charm.land/glamour/v2 | v2.0.1 (go.mod:15) | v2.0.1 | 2026-06-12 | none | Keep |
| github.com/charmbracelet/x/ansi | v0.11.9 (go.mod:87, indirect) | v0.11.9 | 2026-10-08 | none | Keep; tidy decides |
| github.com/charmbracelet/colorprofile | v0.4.3 (go.mod:85, indirect) | v0.4.3 | 2026-03-09 | none | Keep; tidy decides |
| github.com/charmbracelet/ultraviolet | v0.0.0-20261008173134-6b8d4baf91b4 (go.mod:86, indirect) | v0.0.0-20261023060817-a45b7c86333f (no tag) | 2026-10-23 | none; GHSA-qc58 names it in a repro stack only | Follows go mod tidy (Step 3); record the printed pin |
| charm.land/bubbles/v2 | not in go.mod | v2.2.1 | 2026-08-24 | none | Not added here; add at v2.2.1 in the step that imports it |
| github.com/hexops/gotextdiff | v1.0.3 (go.mod:167, indirect) | v1.0.3 (only version) | 2020-12-13 | none; upstream dormant | Keep indirect; no direct import |
| github.com/containerd/errdefs | v1.0.0 (go.mod:18) | v1.0.0 | 2024-10-08 | none | Keep; add table row |
| github.com/coder/websocket | v1.8.15 (go.mod:17) | v1.8.15 | 2026-06-15 | none | Keep |
| github.com/landlock-lsm/go-landlock | v0.11.0 (go.mod:19) | v0.11.2 | 2026-11-10 | GO-2024-3199 fixed before v0.11.0; not affected | Raise to v0.11.2 (Step 3) |
| github.com/moby/moby/api | v1.56.1 (go.mod:20) | v1.56.1 | 2026-10-01 | none for module path | Keep; add table row |
| github.com/moby/moby/client | v0.6.2 (go.mod:21) | v0.6.2 | 2026-10-08 | none | Keep |
| github.com/modal-labs/modal-client/go | v0.11.1 (go.mod:22) | v0.11.1 | 2026-10-16 | none | Keep |
| github.com/pelletier/go-toml/v2 | v2.4.3 (go.mod:23) | v2.4.3 | 2026-07-05 | none | Keep |
| github.com/sandbox0-ai/sdk-go | v0.10.5 (go.mod:24) | v0.10.5 | 2026-10-30 | none | Keep |
| github.com/santhosh-tekuri/jsonschema/v6 | v6.0.3 (go.mod:25) | v6.0.3 | 2026-06-28 | none | Keep |
| github.com/superfly/sprites-go | v0.2.2 (go.mod:26) | v0.2.2 | 2026-10-20 | none | Keep |
| go.starlark.net | v0.0.0-20261021141710-4f4267fb140d (go.mod:27) | same | 2026-10-21 | none | Keep |
| golang.org/x/net | v0.61.0 (go.mod:28) | v0.62.0 | 2026-10-27 | GO-2026-6860 and GO-2026-6866 affect v0.61.0; fixed in v0.62.0 | Raise to v0.62.0 (Step 2) |
| golang.org/x/sync | v0.24.0 (go.mod:29) | v0.25.0 | 2026-10-15 | none | Raise to v0.25.0 (Step 3) |
| golang.org/x/sys | v0.49.0 (go.mod:30) | v0.50.0 | 2026-11-06 | none at either version | Raise to v0.50.0 (Step 3) |
| golang.org/x/text | v0.43.0 (go.mod:31) | v0.44.0 | 2026-11-09 | none in OSV; v0.44.0 commit 367ce9e cites CVE-2026-97044, no OSV record | Raise to v0.44.0 (Step 2) |
| google.golang.org/protobuf | v1.36.12 (go.mod:32) | v1.36.12 | 2026-08-10 | none | Keep |
| modernc.org/sqlite | v1.62.0 (go.mod:33) | v1.62.0 | 2026-11-07 | none | Keep |
| github.com/evilmartians/lefthook/v2 | v2.2.2 (go.mod:8 tool; go.mod:108 indirect) | v2.2.2 | 2026-10-13 | none | Keep |
| github.com/golangci/golangci-lint/v2 | v2.15.0 (go.mod:9 tool; go.mod:144 indirect) | v2.15.0 | 2026-10-16 | none | Keep |
| golang.org/x/vuln | v1.9.0 (go.mod:10 tool; go.mod:304 indirect) | v1.9.0 | 2026-10-11 | none | Keep |
| github.com/google/codesearch | not in go.mod | v1.2.0 | 2020-03-29 | none (OSV list page, GET: no results) | Not added here; add at v1.2.0 in the step that imports it |
| github.com/odvcencio/gotreesitter | not in go.mod | v0.57.0 | 2026-11-08 | none (OSV list page, GET: no results) | Not added here; add at v0.57.0 in the step that imports it |
| github.com/charmbracelet/x/vt | not in go.mod | v0.1.0 | 2026-10-12 | none (OSV list page, GET: no results) | Test-only; the terminal workstream, Step 3 adds it at v0.1.0 |
| github.com/charmbracelet/x/exp/ordered | not in go.mod | v0.1.0 | 2024-06-17 | not checked | Indirect through x/vt; go mod tidy adds it |
| gopkg.in/yaml.v3 | v3.0.1 (indirect) | v3.0.1 | 2022-05-27 | not checked | Keep; gotreesitter requires v3.0.1 |
| pnpm | 12.18.0 (web/package.json:6; .github/workflows/ci.yml:40) | 12.19.0 on GitHub and endoflife.date; npm latest tag still 12.18.0 | 2026-11-10 (12.19.0) | none for 12.18.0 or 12.19.0 | Keep 12.18.0 (contingency) |
| svelte | 5.57.3 (web/package.json:19) | 5.57.3 | 2026-10-26 | 10 GHSAs, all fixed before 5.57.3 | Keep |
| @sveltejs/kit | 3.1.1 (web/package.json:24) | 3.2.0 | 2026-11-10 | Oct 29 batch fixed in 3.1.1; 3.2.0 outside all ranges | Raise to 3.2.0 (Step 4) |
| @sveltejs/adapter-static | 4.0.1 (web/package.json:23) | 4.0.1 | 2026-10-29 | GHSA-jf9x-2fg2-hx4w fixed in 4.0.1 | Keep |
| @sveltejs/vite-plugin-svelte | not pinned (absent) | 7.3.1 | 2026-09-23 | none | Not added here; add at 7.3.1 in the step that needs it |
| svelte-check | 4.7.8 (web/package.json:26) | 4.7.8 | 2026-11-01 | none | Keep |
| typescript | 6.0.3 (web/package.json:27) | 6.0.3 on 6.x; npm latest tag is 7.0.2 | 2026-04-17 | none | Keep 6.0.3 (web-toolchain invariant) |
| vite | 8.3.6 (web/package.json:28) | 8.3.8 | 2026-11-11 | 8.3.x advisories fixed in 8.3.3; 8.3.6 and 8.3.8 outside | Raise to 8.3.8 (Step 4) |
| vitest | 5.1.0 (web/package.json:29) | 5.1.0 | 2026-11-02 | @vitest/mocker advisories outside 5.1.0 | Keep |
| @biomejs/biome | 2.6.6 (web/package.json:22) | 2.6.6 | 2026-11-06 | none | Keep |
| Node.js (CI) | 26.13.0 (.github/workflows/ci.yml:36) | 26.13.0 | 2026-10-28 | 2026-11-11 security releases for 26.x not in 26.13.0 (high: CVE-2026-105482, CVE-2026-105483, CVE-2026-106069, CVE-2026-111497) | Keep 26.13.0 until the Pins steps find a fixed 26.x release |
| happy-dom | not in web/package.json | 21.0.4 | 2026-10-20 | GHSA-834v-8h66-3h79 fixed in 21.0.4 | Not added here; add at 21.0.4 in the step that imports it |
| @testing-library/svelte | not in repo | 5.4.2 | 2026-06-23 | none | Add later at 5.4.2 |
| @testing-library/dom | not in repo | 10.4.2 | 2026-09-13 | none | Add later at 10.4.2 |
| @testing-library/user-event | not in repo | 14.6.9 | 2026-11-10 | none | Add later at 14.6.9 |
| @fontsource-variable/bricolage-grotesque | not in repo | 5.3.0 | 2026-07-19 | none | Add later at 5.3.0 |
| @fontsource-variable/hanken-grotesk | not in repo | 5.3.0 | 2026-07-19 | none | Add later at 5.3.0 |
| pretendard | not in repo | 1.3.9 | 2023-11-05 | none | Add later at 1.3.9 |
| ajv | not in repo | 8.20.0 | 2026-04-24 | none at 8.20.0 | Add later at 8.20.0 |
| marked | not in repo | 18.2.0 | 2026-11-09 | none at 18.2.0 | Add later at 18.2.0 |
| dompurify | not in repo | 3.4.17 | 2026-10-12 | none at 3.4.17 (OSV page 1) | Add later at 3.4.17 |
| highlight.js | not in repo | 11.12.0 | 2026-08-12 | none | Add later at 11.12.0 |
| Nerd Fonts Symbols Only (design) | not in repo | 3.5.1 | 2026-08-21 | not queried | Design source at 3.5.1 |
| @radix-ui/colors (design) | not in repo | 3.0.0 | 2023-10-02 | none | Design source at 3.0.0 |
| Flexoki (design) | not in repo | 2.0.0 | 2025-01-07 | not queried (no registry record) | Design source at 2.0.0 |
| github.com/aymanbagabas/go-udiff (later phase) | not in go.mod (go.sum:75-76) | v0.4.1 | 2026-03-08 | not checked | Add later at v0.4.1 in the step that imports it |

Release channels: Go modules use `https://proxy.golang.org/<module>/@latest`. The Go toolchain uses `https://go.dev/dl/?mode=json` and `https://go.dev/doc/devel/release`. npm packages use `https://registry.npmjs.org/<package>/latest`. GitHub releases use `https://github.com/<owner>/<repo>/releases.atom` and `https://github.com/<owner>/<repo>/releases/tag/<tag>`. Node.js uses `https://nodejs.org/dist/index.json` and `https://nodejs.org/en/blog/release`. Support windows use `https://endoflife.date/api/v1/products/<product>/`. Advisories come from OSV list pages `https://osv.dev/list?q=<module>&ecosystem=<Go|npm>`, OSV records `https://osv.dev/vulnerability/<ID>` and `https://api.osv.dev/v1/vulns/<ID>`, GitHub advisories `https://github.com/<owner>/<repo>/security/advisories`, and Node.js pages `https://nodejs.org/en/blog/vulnerability/`. The grounding used GET requests. It did not run the `POST https://api.osv.dev/v1/querybatch` call that the policy text names.

#### Pins step 1. Raise the Go toolchain to go1.27.4
Anchor: Tony Hoare

Edits:
- `go.mod`, line 5, by hand. Old: `toolchain go1.27.3`. New: `toolchain go1.27.4`. Line 3 (`go 1.27.0`) stays.

Commands, in the repo root:
```
go mod tidy
```
Do not run `go get toolchain@...`. The toolchain line is set by the hand edit above. `go mod tidy` checks the module files after the edit.

API changes and call sites:
- go1.27.4 lists no API change.
- GO-2026-6848, GO-2026-6850, GO-2026-6858 reject some `#cgo` and `//go:cgo_*` inputs. A search for `import "C"`, `#cgo`, and `//go:cgo_` finds no match. Not affected.
- GO-2026-6859 concerns the HTTP/1 `MaxBytesReader` header race. A search for `MaxBytesReader` finds no match. Not affected.
- `.github/workflows/ci.yml:32` and `.github/workflows/box-image.yml:26` read `go-version-file: go.mod`. CI picks up go1.27.4 with no workflow edit.
- `deploy/box/Dockerfile:1` uses `debian:stable-slim`, not a Go image. No edit.

Proof:
- `go version` prints `go1.27.4`.
- `just verify`.

#### Pins step 2. Raise golang.org/x/net and golang.org/x/text (advisory group)
Anchor: Richard Feynman

Edits:
- `go.mod`, line 28. Old: `	golang.org/x/net v0.61.0`. New: `	golang.org/x/net v0.62.0`.
- `go.mod`, line 31. Old: `	golang.org/x/text v0.43.0`. New: `	golang.org/x/text v0.44.0`.

Commands:
```
go get golang.org/x/net@v0.62.0
go get golang.org/x/text@v0.44.0
go mod tidy
```

API changes and call sites:
- x/net. The repo imports `golang.org/x/net/html` and `html/atom` at `internal/tool/webfetch/fetch.go:22-23`. The code uses `html.Tokenizer` and `atom.Lookup` at `fetch.go:335-339`. It never calls `html.Parse`. The named changes touch tree construction (`addText`, `fosterParent`, `childTextNodesAreLiteral`) and quic. Not affected. The repo imports no quic package.
- x/text. The only import is `unicode/norm` at `internal/agent/advisor/guard.go:9`. The two named changes touch `internal/export/idna` and `language.ParseAcceptLanguage`. The repo uses neither (`idna.` and `ParseAcceptLanguage` find no match). Not affected by the named changes. Commit 367ce9e names no package. The targeted test below covers `guard.go`.

Proof:
- `go test -race -count=1 ./internal/tool/webfetch/... ./internal/agent/advisor/...`
- `just verify`.

#### Pins step 3. Raise lipgloss, go-landlock, x/sync, and x/sys; tidy settles ultraviolet
Anchor: Herbert Simon

Edits:
- `go.mod`, line 16. Old: `	charm.land/lipgloss/v2 v2.0.6`. New: `	charm.land/lipgloss/v2 v2.1.0`.
- `go.mod`, line 19. Old: `	github.com/landlock-lsm/go-landlock v0.11.0`. New: `	github.com/landlock-lsm/go-landlock v0.11.2`.
- `go.mod`, line 29. Old: `	golang.org/x/sync v0.24.0`. New: `	golang.org/x/sync v0.25.0`.
- `go.mod`, line 30. Old: `	golang.org/x/sys v0.49.0`. New: `	golang.org/x/sys v0.50.0`.
- `docs/spec/sandbox.md`, line 34. Old: `When the go-landlock v0.11.0 ABI query`. New: `When the go-landlock v0.11.2 ABI query`. The rest of the line stays.

Commands:
```
go get charm.land/lipgloss/v2@v2.1.0
go get github.com/landlock-lsm/go-landlock@v0.11.2
go get golang.org/x/sync@v0.25.0
go get golang.org/x/sys@v0.50.0
go mod tidy
go list -m github.com/charmbracelet/ultraviolet
```
The indirect pin in `go.mod:86` and `go.sum:111-112` is set by tidy only. Do not edit it by hand. The grounding expects `a45b7c86333f`. Tidy keeps `6b8d4ba` if no required module needs the newer version. Do not record it. The Step 5 table lists direct modules only.

API changes and call sites:
- lipgloss v2.1.0. Call sites: `internal/tui/render.go:14` (`NewStyle().Faint`), `internal/tui/render.go:216` (`Wrap`), `internal/tui/model.go:716` (`Width`), `internal/tui/model_test.go:377` (`Width`), `internal/tui/advisor_test.go:98` (`Width`). The named changes are `RGBColor` (16-bit), `LightDark` on CIELAB, additive `Table.FitContent`, and the `NO_COLOR` init query. A search for `lipgloss.` finds only the five sites above. None uses `RGBColor`, `LightDark`, or `Table`. Not affected. Color classification can change terminal output. No test checks it.
- go-landlock v0.11.2. Imports at `internal/runner/sandbox_linux.go:12-13`. The call is `RestrictPaths` at `:57`. `NewConfig` and `MustConfig` (deprecated in v0.11.2) have no match. Not affected. The spec sentence describes ABI behavior, and the sandbox tests below prove it at the new version.
- x/sync. Imports: `singleflight` at `internal/agent/host.go:19` and `errgroup` at `internal/runner/bootstrap.go:23`. The named change renames `syncmap.DeepCopyMap`. A search for `DeepCopyMap` finds no match. Not affected.
- x/sys. Repo call sites include `internal/runner/sandbox_linux.go:14` and `internal/runner/process_windows.go:19`. The named Windows fixes sit inside the generated wrappers. The repo calls no PSAPI accessor (`PSAPI_WORKING_SET` finds no match). `NewNTUnicodeString` finds no match. No call site changes. The Windows and macOS cross-builds below prove the build.

Proof:
- `go test -race -count=1 ./internal/tui/... ./internal/runner/... ./internal/agent/...`
- `GOOS=windows go build ./...`
- `GOOS=darwin go build ./...`
- `just verify`.

#### Pins step 4. Raise @sveltejs/kit to 3.2.0 and vite to 8.3.8
Anchor: David Parnas

Edits:
- `web/package.json`, line 24. Old: `"@sveltejs/kit": "3.1.1",`. New: `"@sveltejs/kit": "3.2.0",`.
- `web/package.json`, line 28. Old: `"vite": "8.3.6",`. New: `"vite": "8.3.8",`.
- `web/pnpm-lock.yaml`: `pnpm install` rewrites it. The lock names kit 3.1.1 at lines 178, 928, and 930. Do not edit the lock by hand.

Commands:
```
cd web && pnpm install
```
Then run `just web`. It runs `pnpm install --frozen-lockfile`, so the lock must match the manifest.

API changes and call sites:
- @sveltejs/kit 3.2.0. The changes are `__data.json` for missing routes, `paths.relative` data-request resolution, and `resolve` rejecting URL inputs. The web imports only `@sveltejs/kit/vite` at `web/vite.config.ts:2`, inside `sveltekit({...})` at `:8`. A search for `$app/`, `@sveltejs/kit`, `__data`, and `paths` in `web/src` finds no match. The `resolve(` matches in `web/src/lib/protocol.ts:113` and `:143` are Promise resolves. Not affected.
- vite 8.3.8. The CSS entrypoint rejection applies when `build.cssCodeSplit` is false. `web/vite.config.ts` sets no `build` option, and a search for `cssCodeSplit` finds no match. The other changes are internal. Not affected.
- Peers. kit 3.2.0 peers `vite ^8.0.12` and `typescript ^6.0.0`. vite 8.3.8 and TypeScript 6.0.3 satisfy them.

Proof:
- `just web`.
- `just web-check`. It runs Biome, Vitest, and svelte-check. The Vitest run proves that tests load with TypeScript 6.0.3 under kit 3.2.0. The web-toolchain Evidence section records the 3.1.1 case.
- `just verify`.

#### Pins step 5. Rewrite the grounded pins in both decision files
Anchor: Dieter Rams

Edits:
- `docs/decisions/dependencies.md`, lines 13-39 (from `## Grounded pins` through the last table row, `lefthook`). Replace them with the block under the heading "New text for `docs/decisions/dependencies.md`, lines 13-39" of this workstream. Line 40 (blank) and `## Dropped` at line 41 stay unchanged. The rewrite removes the `charm.land/bubbles/v2` row (line 25) and the `github.com/modelcontextprotocol/go-sdk` row (line 29). It fixes the x/sys row (line 31).
- `docs/decisions/web-toolchain.md`: insert the `## Pins on 2026-11-11` block from the heading "New section for `docs/decisions/web-toolchain.md`" of this workstream after line 8 (the blank line after the Scope line) and before `## Evidence` (line 9). The Invariant (line 5) and the Evidence text (line 11) stay unchanged.

Commands:
```
grep -rnE 'go1[.]27[.]3|v0[.]49[.]0|v0[.]61[.]0|v0[.]43[.]0|v0[.]24[.]0|v0[.]11[.]0|8[.]3[.]6' docs/decisions docs/spec
```
Expected: no match in `docs/decisions/dependencies.md` or `docs/decisions/web-toolchain.md`. `docs/spec/sandbox.md` has no match after Step 3. The text `3.1.1` in the web-toolchain Evidence section stays as the 2026-11-08 record.

API changes and call sites: none. These files do not compile.

Proof:
- The grep command above.
- `just verify`.

#### New text for `docs/decisions/dependencies.md`, lines 13-39

Pins step 5 writes this text.

```markdown
## Grounded pins

Grounded on 2026-11-11. Each pin equals the latest stable version on its release channel on that date. The Go toolchain row names the newest stable release on `https://go.dev/dl/?mode=json`.

The release channel for Go modules is `https://proxy.golang.org/<module>/@latest`. The support window for Go is `https://endoflife.date/api/v1/products/go/`. Go 1.27 and Go 1.26 are supported. No end-of-life date is set.

Advisories come from OSV list pages, `https://osv.dev/list?q=<module>&ecosystem=Go`, and from the GitHub security advisory pages of each project. No OSV record that the grounding checked lists a pin in this table as affected.

This section lists the modules that `go.mod` requires directly, and the three tools. Indirect modules follow `go mod tidy`.

|Module|Pin|Released|Use|
|---|---|---|---|
|Go toolchain|go1.27.4|2026-11-10|newest supported major; CI reads it from go.mod|
|charm.land/bubbletea/v2|v2.1.1|2026-10-28|TUI|
|charm.land/glamour/v2|v2.0.1|2026-06-12|TUI markdown|
|charm.land/lipgloss/v2|v2.1.0|2026-11-09|TUI|
|github.com/coder/websocket|v1.8.15|2026-06-15|websocket|
|github.com/containerd/errdefs|v1.0.0|2024-10-08|Docker runner error types|
|github.com/landlock-lsm/go-landlock|v0.11.2|2026-11-10|Linux write confinement|
|github.com/moby/moby/api|v1.56.1|2026-10-01|Docker runner API types|
|github.com/moby/moby/client|v0.6.2|2026-10-08|Docker runner|
|github.com/modal-labs/modal-client/go|v0.11.1|2026-10-16|Modal runner|
|github.com/pelletier/go-toml/v2|v2.4.3|2026-07-05|config|
|github.com/sandbox0-ai/sdk-go|v0.10.5|2026-10-30|Sandbox0 runner|
|github.com/santhosh-tekuri/jsonschema/v6|v6.0.3|2026-06-28|protocol schema tests only|
|github.com/superfly/sprites-go|v0.2.2|2026-10-20|Sprites runner|
|go.starlark.net|v0.0.0-20261021141710-4f4267fb140d|2026-10-21|scripts|
|golang.org/x/net|v0.62.0|2026-10-27|HTML tokenizing in web fetch|
|golang.org/x/sync|v0.25.0|2026-10-15|errgroup and singleflight|
|golang.org/x/sys|v0.50.0|2026-11-06|OS calls|
|golang.org/x/text|v0.44.0|2026-11-09|Unicode normalization in advisor guard|
|google.golang.org/protobuf|v1.36.12|2026-08-10|encoding/protowire only|
|modernc.org/sqlite|v1.62.0|2026-11-07|journal mirror, pure Go|
|github.com/evilmartians/lefthook/v2|v2.2.2|2026-10-13|hook tool|
|github.com/golangci/golangci-lint/v2|v2.15.0|2026-10-16|lint tool|
|golang.org/x/vuln|v1.9.0|2026-10-11|govulncheck tool|
```

#### New section for `docs/decisions/web-toolchain.md`

Pins step 5 writes this text.

```markdown
## Pins on 2026-11-11

Grounded on 2026-11-11. Each row gives the exact pin in the repository. Each pin equals the latest stable release, except pnpm and TypeScript. pnpm stays at 12.18.0 while the npm stable tag names it. TypeScript stays at 6.0.3 by the invariant above.

|Package|Pin|Released|Use|
|---|---|---|---|
|pnpm (`web/package.json`, `.github/workflows/ci.yml`)|12.18.0|not recorded|package manager and CI setup|
|Node.js (`.github/workflows/ci.yml`)|26.13.0|2026-10-28|CI runtime. The 2026-11-11 Node.js security releases for 26.x are not in this pin. The engineer who runs the Pins steps reads `https://nodejs.org/dist/index.json` at the start of those steps and once more before the last commit of the plan, and raises this pin to the first 26.x release that carries the fixes. If none is listed at the last commit, the final report says so.|
|svelte|5.57.3|2026-10-26|UI framework|
|@sveltejs/kit|3.2.0|2026-11-10|app framework and build|
|@sveltejs/adapter-static|4.0.1|2026-10-29|static build output|
|svelte-check|4.7.8|2026-11-01|Svelte type check|
|typescript|6.0.3|2026-04-17|type check. The pin MUST stay at 6.0.3.|
|vite|8.3.8|2026-11-11|bundler|
|vitest|5.1.0|2026-11-02|unit tests|
|@biomejs/biome|2.6.6|2026-11-06|lint and format|
```

