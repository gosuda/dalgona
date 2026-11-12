# Web toolchain

## Invariant

The web UI MUST pin TypeScript 6.0.3. It MUST NOT use TypeScript 7.0.2 with the pinned Svelte tooling.

Scope: `web/package.json` and `web/pnpm-lock.yaml`. Enforcement: `just web` installs dependencies with `pnpm --frozen-lockfile` and builds the assets. `just web-check` runs Biome, Vitest, and svelte-check with the installed tools.

## Grounded pins

Grounded on 2026-11-12. Each row gives an exact devDependency pin in `web/package.json`. The release date is the `time` entry of that version in `https://registry.npmjs.org/<package>`. The OSV column holds the result of `POST https://api.osv.dev/v1/query` for that package and version, run on 2026-11-12.

|Package|Pin|Released|Use|OSV 2026-11-12|
|---|---|---|---|---|
|happy-dom|21.0.4|2026-10-20|DOM environment of the `dom` Vitest project|none|
|@testing-library/svelte|5.4.2|2026-06-23|renders Svelte components in tests, with its `svelteTesting` Vite plugin|none|
|@testing-library/dom|10.4.2|2026-09-13|queries that `@testing-library/svelte` uses|none|
|@testing-library/user-event|14.6.9|2026-11-10|user input in component tests|none|
|ajv|8.20.0|2026-04-24|JSON Schema check of the parsers in the schema test|none|
|@fontsource-variable/bricolage-grotesque|5.3.0|2026-07-19|Bricolage Grotesque for titles and control labels|none|
|@fontsource-variable/hanken-grotesk|5.3.0|2026-07-19|Hanken Grotesk for body text|none|
|pretendard|1.3.9|2023-11-05|Hangul fallback, loaded by unicode-range subset|none|

## Pins on 2026-11-11

Grounded on 2026-11-11. Each row gives the exact pin in the repository. Each pin equals the latest stable release, except pnpm and TypeScript. pnpm stays at 12.18.0 while the npm stable tag names it. TypeScript stays at 6.0.3 by the invariant above. The OSV column holds the result of `POST https://api.osv.dev/v1/querybatch` for each npm pin, run on 2026-11-12. The Node.js row is not an npm package and was not sent to that query.

|Package|Pin|Released|Use|OSV batch 2026-11-12|
|---|---|---|---|---|
|pnpm (`web/package.json`, `.github/workflows/ci.yml`)|12.18.0|not recorded|package manager and CI setup|none|
|Node.js (`.github/workflows/ci.yml`)|26.13.1|2026-11-11|CI runtime. Raised from 26.13.0 on 2026-11-12 to the 26.x release that the Node.js security release page of 2026-11-11 lists as Latest LTS, with the 26.x fixes. The CVE IDs that the plan named (CVE-2026-105482, CVE-2026-105483, CVE-2026-106069, CVE-2026-111497) are not on that page, so those IDs are not confirmed fixed.|not queried|
|svelte|5.57.3|2026-10-26|UI framework|none|
|@sveltejs/kit|3.2.0|2026-11-10|app framework and build|none|
|@sveltejs/adapter-static|4.0.1|2026-10-29|static build output|none|
|svelte-check|4.7.8|2026-11-01|Svelte type check|none|
|typescript|6.0.3|2026-04-17|type check. The pin MUST stay at 6.0.3.|none|
|vite|8.3.8|2026-11-11|bundler|none|
|vitest|5.1.0|2026-11-02|unit tests|none|
|@biomejs/biome|2.6.6|2026-11-06|lint and format|none|

## Evidence

Observed on 2026-11-08. SvelteKit 3.1.1 reads `ts.sys` at startup. TypeScript 7.0.2 does not expose `ts.sys`, so Vitest failed before it loaded a test. TypeScript 6.0.3 is the newest 6.x release that the SvelteKit 3.1.1 and svelte-check 4.7.8 peer ranges accept. OSV `POST https://api.osv.dev/v1/query` for npm `typescript` 6.0.3 returned `{}` on that date. The approved plan names this contingency.
