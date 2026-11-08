# Web toolchain

## Invariant

The web UI MUST pin TypeScript 6.0.3. It MUST NOT use TypeScript 7.0.2 with the pinned Svelte tooling.

Scope: `web/package.json` and `web/pnpm-lock.yaml`. Enforcement: `just web-check` runs Biome, Vitest, and svelte-check on the frozen lockfile.

## Evidence

Observed on 2026-11-08. SvelteKit 3.1.1 reads `ts.sys` at startup. TypeScript 7.0.2 does not expose `ts.sys`, so Vitest failed before it loaded a test. TypeScript 6.0.3 is the newest 6.x release that the SvelteKit 3.1.1 and svelte-check 4.7.8 peer ranges accept. OSV `POST https://api.osv.dev/v1/query` for npm `typescript` 6.0.3 returned `{}` on that date. The approved plan names this contingency.
