# DESIGN: the dal and dalgona design system

## Color

- Components reference opaline roles, never raw values. Roles: `text`, `dim`, `faint`
  (decoration only, never a sole carrier), `accent`, `success`, `warning`, `error`;
  fills `surface`, `selected`, `border`. `canvas` is the terminal background. One accent
  hue. Status hues are muted, never saturated primaries. No gradients. No purple-blue or
  purple-pink. No self-generated colors. No colored fill behind small text except the
  neutral `selected` fill. Default theme pair: `flexoki-dark` and `flexoki-light`.
  `theme = "auto"` queries OSC 11 (300 ms), buckets by luminance at or below 0.2,
  subscribes to DECRPM 2031, falls back to `COLORFGBG`, then to terminal-palette mode.
  Never guess a contrast you cannot compute. Shipped set: exactly the nine built-ins
  `flexoki-dark`, `flexoki-light`, `github-dark`, `github-light`, `kanagawa-wave`,
  `catppuccin-mocha`, `rose-pine`, `ayu-mirage`, `catppuccin-latte`. A theme ships only after the contrast gate passes in CI: each of `text`, `dim`, `accent`, `success`,
  `warning`, `error` at least 4.5 against each of `canvas`, `surface`, `selected`;
  `faint` at least 3.0 against `canvas`. A failing theme is excluded or gets a same-
  theme role override. 256-color maps to the nearest Euclidean RGB match. 16-color maps
  to terminal brights; never estimate its contrast. `NO_COLOR` strips all color SGR and
  keeps bold, dim, reverse, underline. Meaning never depends on color alone: every
  status is a word, every selection is a marker, diffs keep their signs.

## Typography

- The terminal owns the font. dal owns emphasis: `normal`, `bold`, `dim`, `reverse`,
  `underline` only. No italics anywhere. At most two emphasis levels per region:
  transcript is normal plus bold, status is normal plus accent. Underline is reserved
  for links (OSC 8). Chrome is ASCII or braille only: `...` for ellipsis, ASCII tree and
  borders, braille spinner. The one measured Ambiguous chrome glyph is ` · ` between
  status segments. Content prose may use U+2026 and smart quotes. Numeric status slots
  are fixed width and right aligned: a value change rewrites its own cells and never
  shifts neighbors. Durations: `{ms} ms` under 1 s, `{s}s` one decimal under 120 s, else
  `{m}m{ss}s`. Tokens as `{n}k` from 1000. All width decisions go through `ui::width`
  (`unicode-segmentation` + `unicode-width` 0.2, `cjk` feature): cluster width is the
  maximum member width; Ambiguous is 1 narrow and 2 in CJK mode; VS16 and ZWJ sequences
  are 2; each regional indicator is 2; jamo clusters are 2; control characters are
  escaped; invalid UTF-8 substitutes one U+FFFD per bad lead byte. No bidi reordering.

## Space

- Every transcript row reserves a 2-cell left gutter. Live block height: at most
  `min(height - 4, 16)` rendered rows. Region budgets by terminal height: 28 or more
  gives notices 3, activity 8, composer 1-8, hint 1, status 1; 24 gives 2, 5, 1-6, 1, 1;
  12 gives 1, 2, 1-3, 0, 1. Below 8 rows, render status plus `dalgon needs at least 8
  rows`. Prose measure cap by width: 80-119 uses `width - 8`; 60-79 uses `width - 6`;
  40-59 uses `width - 4`; 20-39 uses `width - 2`; under 20 uses `width - 2`. Code
  blocks, diffs, and tables render full width. Collapse late, in the named order:
  notices merge to one line; activity folds older rows; composer clamps; hint folds into
  the status right side. Nothing is ever clipped without a named expand path. There is
  no radius, shadow, or elevation vocabulary in a terminal. Layering is the region
  order. Borders are ASCII and structural only.

## Density and motion

- Density is medium-high: prose at the measure cap, one row per chrome element, nothing
  decorative occupies a row. Streaming deltas coalesce at 33 ms, at most 60 Hz; a key
  event requests an immediate frame that bypasses the batch. `DAL_NO_MOTION` freezes the
  spinner on its first frame and disables all frame animation; state stays carried by
  words. Every animated state also leaves a static cue. Motion is never the only
  channel.

## Components

All components render from `Update` values and structured `View` data only; none reads
the journal. Full rendering rules: `design-tui.md` section 5.

| Shape | Contract |
|---|---|
| Tool card | Spinner/status word, name, summary, outcome, duration. `ctrl+e` expands. |
| Aggregation rows | Failed first, then running, then fastest finished. Caps 8/25. |
| Diff | Unified; `+` success, `-` error, `@@` dim; 20 rows cap. |
| Diagram card | Art at 1 cell/glyph with role colors, or source fallback. |
| Image card | Placeholder card; Kitty/sixel opt-in, off under multiplexers. |
| Thinking | Collapsed one-liner; expanded 10 dim rows cap. |
| Notice | `note: {text}`, at most 3, never modal, journaled. |
| Status line | Always painted; segments drop whole from the right. |
| Composer | `ratatui-textarea`; `/` and `$` completion lanes. |
| Dialogs and pickers | Take composer+hint rows; reverse-video `>` focus; Esc closes. |

Detail that must survive: settled tool cards read `ok  {name} {summary} · {dur}`
or `failed  {name} · {reason} · {dur}`; aggregation folds `({n} more:
{breakdown})`; status slots left to right are spinner + state word, model id,
`~` path + `(branch)`, `in {in} out {out}`, `ctx {pct}%`, agents/jobs count,
cost — state words `thinking`, `working`, `fetching`, `compacting`, `retrying`,
`ctx` warns at 70% and errors at 90%; composer lanes: `/` lists commands with
`skill:*` filtered out, `$` opens a skills-only lane with the same match and
ranking, `$<name>` or `$<ns>:<name>` submits as a skill invocation identical to
`/skill:<name>`; diagram fallback renders `diagram {kind}: render failed
({reason}) · source shown` within a 2 s off-thread budget.

## Keys

One key map for both modes; conflicts resolve by ownership order: overlay dialogs, then
app, then composer, then editor. The legacy column (no kitty protocol) is the contract
for a non-kitty terminal; never leave a binding kitty-only. Full map: `design-tui.md`
section 8.

A dialog never takes consent from type-ahead. Each request dialog starts locked: for
500 ms after its first frame it drops every key but an unmodified Esc, which only denies
or dismisses. The hint row reads `esc denies · answer keys ready in a moment` while
locked and lists the answer keys (`y allow · a session · n deny · v view · esc denies`
for an approval or grant) only once they act, so a user or a gate that sees the keys can
press one. Dropped keys are never queued. A request that follows another starts locked
again. Only a key pressed after the answer keys are on screen can approve.

## Voice

The register details sit in `BRANDING.md`. The style summary: sentence case; verb-first
actions; no exclamation marks; every error names what happened and the fix. A rule fire
reads `rule {name} fired. The {subject} matched /{pattern}/.` exactly once per fire. An
unsent draft blocks quit once: `[y] Discard the draft and quit   [n] Keep editing`.

## Accessibility

Full keyboard reach; Esc closes overlays. Selection is reverse video plus `>`, never
color alone. Every state is a word next to any glyph. `NO_COLOR` and `DAL_NO_MOTION` are
honored. No interaction is mouse-only; inline mode never captures the mouse. OSC 52 copy
reports honestly: `copied {n} characters` or `copy failed: the terminal refused the
selection`.

## Before you commit

A TUI style change is correct when all of these hold:

1. Every role, width, row count, and truncation above is satisfied, or the deviation is
named in `design-tui.md`. 2. The same state reads correctly under `NO_COLOR`,
`DAL_NO_MOTION`, 16-color, and 20-column width. 3. The contrast gate still passes for
every shipped theme. 4. The frame-level invariants F1-F8 in `CONCEPTS.md` still hold on
the changed path.
