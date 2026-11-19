#### Web step 9. Self-hosted fonts and type roles

- Dependencies. `web/package.json` dependencies add `"@fontsource-variable/bricolage-grotesque": "5.3.0"`, `"@fontsource-variable/hanken-grotesk": "5.3.0"`, `"pretendard": "1.3.9"`. Add three rows to the `## Grounded pins` table that Step 1 creates; this step adds no other row.
- `web/src/lib/theme/fonts.css`:
  - `@import "@fontsource-variable/bricolage-grotesque";`
  - `@import "@fontsource-variable/hanken-grotesk";`
  - `@import "@fontsource-variable/hanken-grotesk/wght-italic.css";`
  - `@import "pretendard/dist/web/variable/pretendardvariable-dynamic-subset.css";`
  - `@font-face` for the icon subset (Design record, Icons): `font-family: "Symbols Nerd Subset"`, `src: url("./symbols-nerd-subset.woff2") format("woff2")`. The tests-and-docs workstream, Step 19 writes the file `web/src/lib/theme/symbols-nerd-subset.woff2`.
  - `+layout.svelte` imports `#lib/theme/fonts.css`.
- Type tokens in `scale.ts`.
  - `faceTitle = '"Bricolage Grotesque Variable", "Pretendard Variable", sans-serif'`
  - `faceText = '"Hanken Grotesk Variable", "Pretendard Variable", sans-serif'`
  - `faceMono = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"` (Design record, Type for the web)
- CSS. `html{font-family:var(--face-text)}`; `h1,h2,h3{font-family:var(--face-title);font-weight:var(--weight-title)}`; `button,.tag,.point{font-family:var(--face-title)}` (control labels, Design record, Type for the web). Prose (`.prose`, messages, previews): `word-break: keep-all; overflow-wrap: anywhere`.
- Typography fixes.
  - Delete `--font-interface` (Avenir).
  - `.empty-state h2` uses `var(--size-title)`, replacing the clamp at `Transcript.svelte:472`.
  - The sidebar heading becomes an overline (`--size-caption`, uppercase, `letter-spacing: 0.05em`).
  - `font-variant-numeric: tabular-nums` on counts, `dd`, the elapsed time and the context percent (Design record, Type for the web). Token counts are not shown.
- Licences. Copy the OFL texts to `web/static/licenses/bricolage-grotesque-OFL.txt`, `hanken-grotesk-OFL.txt` and `pretendard-OFL.txt`, from the licence file of each package: `LICENSE` at the package root for `@fontsource-variable/bricolage-grotesque` 5.3.0 and `@fontsource-variable/hanken-grotesk` 5.3.0, and `dist/LICENSE.txt` for `pretendard` 1.3.9, because that package has no licence file at its root. The icon subset gets `web/static/licenses/symbols-nerd-font.txt` for Symbols Nerd Font Mono (MIT), Material Design Icons (Apache-2.0) and Weather Icons (SIL OFL 1.1). The icon subset has no licence text beside it, so the three icon-set texts come from the Nerd Fonts 3.5.1 licence files named in the outside check of the tests-and-docs workstream (Step 19). Weather Icons has no LICENSE file upstream. If a text cannot be read, see the contingency for this workstream. SvelteKit copies `static/` into `internal/webui/dist`.
- Test. Browser check `web/checks/fonts.js`.
  - It renders `<span style="font-family:var(--face-text)">Aa 한글</span>` and `<span style="font-family:var(--face-title)">Aa</span>`, then awaits `document.fonts.ready`.
  - It requires a `FontFace` in `document.fonts` with status `loaded` for families `Hanken Grotesk Variable`, `Bricolage Grotesque Variable` and `Pretendard Variable`.
  - It also requires that no loaded face of `Pretendard Variable` covers only Latin ranges, and that a face named `Symbols Nerd Subset` is `loaded`.
  - Fails before: no `@font-face` exists.
- Spec (`docs/spec/web.md`). New `### Type`: the three faces, the reader's monospace, Hangul through Pretendard dynamic subsets, `keep-all`, and the licence files.
- Lands after: Steps 4 and 8 (`+layout.svelte` and the type tokens).

Anchor: Saul Bass.

