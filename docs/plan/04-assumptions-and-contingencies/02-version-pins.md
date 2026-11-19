### Version pins

- pnpm. `web/package.json:6`, `.github/workflows/ci.yml:40`, and `deploy/cloudflare/package.json:5` all name `pnpm@12.18.0`. The Pins steps keep `12.18.0` and edit none of the three files. When the npm `latest` tag moves, raise all three together. Then run `pnpm install` in `web/` and in `deploy/cloudflare/`, and run `just verify`. The grounding found 12.19.0 on GitHub and endoflife.date. Version 12.19.0 prints warnings for `engines.node` mismatches.
- Node.js. The pin stays at 26.13.0 because nodejs.org listed no 26.x release with the 2026-11-11 security fixes on 2026-11-12. The engineer who runs the Pins steps reads `https://nodejs.org/dist/index.json` at the start of those steps and once more before the last commit of the plan. That engineer raises `.github/workflows/ci.yml:36` to the first 26.x release that carries the fixes. If none is listed at the last commit, the final report says so. Until a fixed release is listed, the pin carries the high-severity issues that the 2026-11-11 fixes address.
- TypeScript. No fallback. TypeScript 7.0.2 MUST NOT be used with the pinned Svelte tooling.
- ultraviolet. If tidy keeps `6b8d4ba`, record that pin in the tables. Do not edit it by hand.
- gotextdiff. The later unified-diff step uses go-udiff v0.4.1, not gotextdiff.
- Incompatibilities found:
  - `docs/decisions/dependencies.md:25` (bubbles) and `:29` (MCP SDK) conflict with go.mod. Step 5 removes them.
  - `docs/decisions/dependencies.md:31` lists x/sys v0.49.0 as latest. Step 5 corrects it.
  - `docs/decisions/dependencies.md:5` and `:6` conflict with the code. `internal/app/cli.go:9` and `:698-711`, `internal/agent/gate.go:7` and `:162-163`, and `internal/model/antigravity/project.go:14` and `:250` use `encoding/json/v2`. This phase does not change that code.
  - `docs/spec/sandbox.md:34` names the go-landlock version. Step 3 updates it.
  - `deploy/cloudflare/package.json:5` pins pnpm 12.18.0. The Pins steps keep that pin, so none of them edits it.
  - The repo root has no changelog file. No changelog step is needed.

