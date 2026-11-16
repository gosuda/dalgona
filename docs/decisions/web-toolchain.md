# Web toolchain

## Invariant

The web UI MUST pin TypeScript 6.0.3. It MUST NOT use TypeScript 7.0.2 with the pinned Svelte tooling.

Scope: `web/package.json` and `web/pnpm-lock.yaml`. Enforcement: `just web` installs dependencies with `pnpm -C web install --frozen-lockfile` and builds the assets. `just web-check` runs Biome, Vitest, and svelte-check with the installed tools.

## Grounded pins

Grounded on 2026-11-12. Each row gives an exact dependency or devDependency pin in `web/package.json`. The release date is the `time` entry of that version in `https://registry.npmjs.org/<package>`. The user-event timestamp was read on 2026-11-15 and is linked below. For unchanged pins, the OSV column retains the result of the `POST https://api.osv.dev/v1/querybatch` request of 2026-11-15 that `dependencies.md` describes. For user-event 14.6.10, it records the supplementary exact-version query described below.

|Package|Pin|Released|Use|OSV batch 2026-11-15|
|---|---|---|---|---|
|happy-dom|21.0.4|2026-10-20|DOM environment of the `dom` Vitest project|none|
|@testing-library/svelte|5.4.2|2026-06-23|renders Svelte components in tests, with its `svelteTesting` Vite plugin|none|
|@testing-library/dom|10.4.2|2026-09-13|queries that `@testing-library/svelte` uses|none|
|@testing-library/user-event|14.6.10|2026-11-12|user input in component tests|none (supplementary query)|
|ajv|8.20.0|2026-04-24|JSON Schema check of the parsers in the schema test|none|
|@sveltejs/vite-plugin-svelte|7.3.1|2026-09-23|Svelte compiler plugin for the Vite build|none|
|@fontsource-variable/bricolage-grotesque|5.3.0|2026-07-19|Bricolage Grotesque for titles and control labels|none|
|@fontsource-variable/hanken-grotesk|5.3.0|2026-07-19|Hanken Grotesk for body text|none|
|pretendard|1.3.9|2023-11-05|Hangul fallback, loaded by unicode-range subset|none|
|marked|18.2.0|2026-11-09|Markdown rendering of model text|none|
|dompurify|3.4.17|2026-10-12|sanitizer for rendered Markdown|none|
|highlight.js|11.12.0|2026-08-12|syntax highlighting of code blocks, core build with a fixed language set|none|

## Pins on 2026-11-11

The original pins equalled the latest stable releases on that date, except pnpm and TypeScript. TypeScript stays at 6.0.3 by the invariant above. pnpm 12.21.0 is pinned in `web/package.json`, `deploy/cloudflare/package.json`, and `.github/workflows/ci.yml`. The npm `latest` tag names pnpm 12.21.0, read on 2026-11-15 and again on 2026-11-16. For unchanged npm pins, the OSV column retains the result of the `POST https://api.osv.dev/v1/querybatch` request of 2026-11-15. Rows marked `supplementary query` record the exact-version queries that the evidence section describes. The Node.js row is not an npm package and was not sent to either query.

|Package|Pin|Released|Use|OSV batch 2026-11-15|
|---|---|---|---|---|
|pnpm (`web/package.json`, `deploy/cloudflare/package.json`, `.github/workflows/ci.yml`)|12.21.0|2026-11-13|package manager and CI setup|none (supplementary query)|
|Node.js (`.github/workflows/ci.yml`)|26.13.1|2026-11-10|CI runtime. `https://nodejs.org/dist/index.json` lists v26.13.1 as the newest 26.x release on 2026-11-15. The page of the 2026-11-11 Node.js security releases names v26.13.1 as the 26.x download. The CVE IDs CVE-2026-105482, CVE-2026-105483, CVE-2026-106069 and CVE-2026-111497 are not on that page, so those IDs are not confirmed fixed.|not queried|
|svelte|5.57.3|2026-10-26|UI framework|none|
|@sveltejs/kit|3.2.0|2026-11-10|app framework and build|none|
|@sveltejs/adapter-static|4.0.1|2026-10-29|static build output|none|
|svelte-check|4.7.8|2026-11-01|Svelte type check|none|
|typescript|6.0.3|2026-04-16|type check. The pin MUST stay at 6.0.3.|none|
|vite|8.3.8|2026-11-11|bundler|none|
|vitest|5.1.0|2026-11-02|unit tests|none|
|@biomejs/biome|2.6.7|2026-11-11|lint and format|none (supplementary query)|

## Evidence

Official npm registry metadata read on 2026-11-15 gives these exact-version publication timestamps (UTC):

- [pnpm registry metadata](https://registry.npmjs.org/pnpm): `time["12.20.0"]` is `2026-11-12T00:23:01.043Z`.
- [Biome registry metadata](https://registry.npmjs.org/%40biomejs%2Fbiome): `time["2.6.7"]` is `2026-11-11T21:44:33.443Z`.
- [user-event registry metadata](https://registry.npmjs.org/%40testing-library%2Fuser-event): `time["14.6.10"]` is `2026-11-12T04:41:23.349Z`.

On 2026-11-15, a supplementary `POST` to the [OSV batch query API](https://api.osv.dev/v1/querybatch) queried the exact npm versions pnpm 12.20.0, @biomejs/biome 2.6.7, and @testing-library/user-event 14.6.10. Their corresponding response entries were `{}`, `{}`, and `{}`: no vulnerability was listed for those exact versions. These results are separate from the earlier batch and do not reuse advisory results for older pins.

Observed on 2026-11-08. SvelteKit 3.1.1 reads `ts.sys` at startup. TypeScript 7.0.2 does not expose `ts.sys`, so Vitest failed before it loaded a test. TypeScript 6.0.3 is the newest 6.x release that the SvelteKit 3.1.1 and svelte-check 4.7.8 peer ranges accept. OSV `POST https://api.osv.dev/v1/query` for npm `typescript` 6.0.3 returned `{}` on that date. TypeScript 7.0.2 MUST NOT be used with the pinned Svelte tooling.

The [pnpm registry metadata](https://registry.npmjs.org/pnpm) gives `time["12.21.0"]` as `2026-11-13T21:01:11.515Z` (UTC). On 2026-11-15, an exact-version `POST` to the [OSV query API](https://api.osv.dev/v1/query) for pnpm 12.21.0 returned `{}`. Earlier empty OSV results for older pnpm pins do not cover 12.21.0, so this query ran for the new pin. On 2026-11-16, the npm `latest` tag names 12.21.0. This result covers the pinned package only and does not clear the dependency tree. The audit of this refresh reports one low advisory for `deepmerge` 4.3.1, which has no patched version.

Observed on 2026-11-15. `pnpm -C web install --frozen-lockfile` passes without `web/pnpm-workspace.yaml`, and `pnpm -C web build` passes in a copy without it. The file listed `vite@8.3.8` in `minimumReleaseAgeExclude`. The repository no longer holds the file.
