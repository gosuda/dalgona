#### Web step 8. Palette, tokens and four themes

- `web/src/lib/theme/palette.ts`
  - `export const colorRoles` lists the twelve Design record, Colour roles: `canvas`, `surface`, `tone`, `line`, `strong`, `text`, `dim`, `faint`, `accentText`, `failedText`, `addedText`, `warningText`. No other colour exists. `faint` is decoration only and never body text. There is no `failed` or `accent` fill role: failure is text (Design record, The point and the thread hue), and the point takes the hue values below.
  - `export const concreteThemes = ["day", "soft-day", "night", "soft-night"] as const`.
  - `export const palette: Readonly<Record<ConcreteTheme, Readonly<Record<ColorRole, `#${string}`>>>>` with every Design record, Colour value. The values are copied from `DESIGN.md`, which the tests-and-docs workstream, Step 19 writes; this file is the code copy (both night grounds are Radix Colors 3.0.0 slate dark, and every other value is Flexoki). `export const hues` lists the four Design record, The point and the thread hue hues (`orange`, `cyan`, `blue`, `magenta`), each with a `point` and an `onPoint` for the night pair and for the day pair; `soft-night` reuses the night values and `soft-day` the day values.
  - `export const contrastRules: readonly { fg: ColorRole; bg: ColorRole; min: number }[]`:
    - 4.5 for `text`, `dim`, `accentText`, `failedText`, `addedText` and `warningText` on `canvas`, `surface` and `tone`. `faint` has no rule: it is decoration only.
    - 4.5 for `on-point` on `point` for each hue in `hues`, and 3 for `point` on `canvas`
    - 3 for `strong` and `text` (focus ring) on `canvas`, `surface` and `tone`
- `web/src/lib/theme/scale.ts` holds non-colour token data (values from `DESIGN.md`):
  - Type: `faceTitle`, `faceText`, `faceMono`; sizes `sizeCaption`, `sizeSmall`, `sizeBody`, `sizeHeading`, `sizeTitle`, `sizeDisplay`; leading `leadingTight`, `leadingBody`, `leadingCode` (unitless, Design record, Type for the web); weights `weightText`, `weightStrong`, `weightTitle`. The values are in the Design record, Scale.
  - Space: `space1`, `space2`, `space3`, `space4`, `space6`, `space8`, where `--space-n` is n times 4 px.
  - Shape: `radiusTag`, `radiusControl`, `radiusCard`, `borderWidth`, `targetMin`, `controlHeight` (36px, Design record, Components), `controlHeightCoarse`.
  - Motion: `motionBreath`, `motionQuick`, `ease`.
- No `css.ts`. `installTokens` in `theme.ts` builds the CSS text itself, in order:
  1. `:root{` with every scale token as a kebab-case custom property `}`.
  2. `:root,:root[data-theme="day"],:root[data-theme="auto"]{` with day colours `}`, and `:root[data-theme="soft-day"],:root[data-theme="auto-soft"]{` with soft-day colours `}`.
  3. `@media (prefers-color-scheme: dark){:root[data-theme="auto"]{` with night colours `}:root[data-theme="auto-soft"]{` with soft-night colours `}}`.
  4. One block each for `night` and `soft-night`. Each block also sets `color-scheme`, and `[data-hue="0"]` to `[data-hue="3"]` set `--point` and `--on-point` from `hues` (Design record, The point and the thread hue).
  5. `@media (prefers-reduced-motion: reduce){:root{--motion-breath:0s;--motion-quick:0s}}`.
  - Role names map camelCase to kebab-case, for example `lineStrong` to `--line-strong`.
- `web/src/lib/theme/contrast.ts`: `relativeLuminance(hex: `#${string}`): number` and `contrastRatio(a, b): number` (WCAG 2.2 formula).
- `web/src/lib/theme/theme.ts`
  - `export const themeSettings = ["auto", "auto-soft", "night", "soft-night", "day", "soft-day"] as const` (Design record, Colour)
  - `parseThemeSetting(value: unknown): ThemeSetting`, where unknown values give `"auto"`
  - `resolveTheme(setting, prefersDark: boolean): ConcreteTheme`: `auto` gives `night` or `day`; `auto-soft` gives `soft-night` or `soft-day`
  - `installTokens(doc: Document): void` appends `<style id="dalgona-tokens">` through `textContent` once, built from `palette`, `scale` and `hues`
  - `applyTheme(setting, doc: Document, storage: Storage): void` sets `doc.documentElement.dataset.theme`, writes `storage.setItem("dalgona.theme", setting)`, and creates `<meta name="theme-color">` when absent and sets its `content` from `palette[resolved].canvas`
- Components (Design record, Components). Buttons, fields, tags and the `Mark` controls follow the Soft ledger rows of the Design record, Browser controls. `CommandCard`, `ToolCard`, `DiffCard` and `ApprovalCard` follow the Ruled ledger rows of the Design record, Browser cards. Panels take the same card rules. Write each rule with the token names above, and use no pixel value that a token already covers. Every value comes from the Design record, and no reference page is needed (cross-phase decision 31). Apply the four Design record, Components fixes while porting: the focus ring (Design record, Colour), night diff rows on `surface` with `addedText` and `failedText` (Step 17 applies them), 36 px controls (the token `controlHeight` is here, and Step 24 applies the rule), and the 2 px `text` bar on the selected row (Step 11 applies it).
- Layout and startup. `+layout.svelte` calls `installTokens(document)` and `applyTheme(parseThemeSetting(localStorage.getItem("dalgona.theme")), document, localStorage)` at module top. After `dalgona/settings/read`, `Session` calls `applyTheme` with key `theme`.
- Migration. `web/src/app.css` drops the primitives and old roles (`app.css:1-52`). Every rule in `app.css` and all components moves to the new names:

  | Old | New |
  |---|---|
  | `--color-bg-page` | `--canvas` |
  | `--color-bg-surface` | `--surface` |
  | `--color-bg-sidebar`, `--color-bg-inset` | `--tone` |
  | `--color-bg-selected` | `--tone` |
  | `--color-text` | `--text` |
  | `--color-text-secondary` | `--dim` |
  | `--color-text-on-accent` | `--on-point` |
  | `--color-border` | `--line` |
  | `--color-border-strong` | `--strong` |
  | `--color-accent` | `--point` |
  | `--color-accent-hover` | deleted: the point has no hover colour, and Design record, Colour names no such role |
  | `--color-focus` | deleted: the focus ring is `--text` (Design record, Colour) |
  | `--color-danger` | `--failed-text` |
  | `--color-danger-surface` | deleted: failure is text, never a fill (Design record, The point and the thread hue) |
  | `--color-success` | `--added-text` |
  | `--color-keyword-surface` | deleted |
  | old `--space-5` (24 px) | `--space-6` |
  | old `--space-6` (32 px) | `--space-8` |

- Other CSS changes.
  - Focus (Design record, Colour): `:focus-visible{outline:2px solid var(--text);outline-offset:2px}` on every focusable control. No rule sets `outline: 0` without this ring. The old rule at `app.css:118-121` moves to `--text`.
  - Point (Design record, The point and the thread hue): `button.primary` becomes `.point`, with `background:var(--point);color:var(--on-point)`. The class and its rule are defined here; the holder rule is in the conventions.
  - `button.quiet` takes `border-color: var(--strong)`. This fixes the 1.56:1 boundary at `app.css:109-112`.
  - Add `@media (forced-colors: active)` rules: `.tag{border:1px solid CanvasText}`, `button.primary{border:2px solid ButtonText}`, `.notices p{border-inline-start:2px solid CanvasText}`.
  - Remove `color-scheme: light` (`app.css:59`). Each theme block sets `color-scheme`.
  - Remove `<meta name="theme-color" content="#f5f2eb">` from `web/src/app.html:6`. Add `<link rel="icon" href="/dalgona.svg" type="image/svg+xml">` (app icon) and `<link rel="icon" href="/dalgona-small.svg" sizes="16x16" type="image/svg+xml">` (favicon). Copy `dalgona.svg` and `dalgona-small.svg` from `assets/brand/` (the tests-and-docs workstream, Step 19) into `web/static/` (Design record, Product marks). This step creates `web/static/`, which does not exist yet.
- Tests.
  - `web/src/lib/theme/contrast.test.ts`:
    - `matches WCAG reference ratios`: black on white is 21; white on white is 1; `#767676` on `#ffffff` is between 4.54 and 4.55. Source: WCAG 2.2 Understanding 1.4.3.
    - `every theme meets its contrast rules`. Fails before: the module is absent, and today's quiet border measures 1.56:1.
  - `theme.test.ts`: `auto follows the system setting`, `an explicit theme ignores the system setting`, `unknown values fall back to auto` (contract: the section "Schemas of the web workstream"), `auto-soft follows the system setting within the soft pair`.
  - `theme.dom.test.ts`: `applyTheme sets data-theme, caches the setting and updates theme-color`.
- Spec (`docs/spec/web.md`). New `### Theme`: the six settings, `auto` and `auto-soft` follow `prefers-color-scheme`, the cache key, forced-colours rules, and the contrast gate.
- Lands after: Step 4 (`+layout.svelte` and `Session.applyTheme`).

Anchor: Otl Aicher, Dieter Rams.

