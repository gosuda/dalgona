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
  `{m}m{ss}s`. Tokens as `{n}k` from 1000. All width decisions go through `width::width`
  (`unicode-segmentation` + `unicode-width` 0.2 `width_cjk`): cluster width is the
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
the journal. All honor the 2-cell left gutter (`Space`). Every state renders at every
terminal width down to 20 columns without clipping that lacks a named expand path.

| Shape | Contract |
|---|---|
| Tool card | Spinner/status word, name, summary, outcome, duration. `ctrl+o` expands. |
| Aggregation rows | Failed first, then running, then fastest finished. Caps 8/25. |
| Diff | Unified; `+` success, `-` error, `@@` dim; 20 rows cap. |
| Diagram card | Art at 1 cell/glyph with role colors, or source fallback. |
| Image card | Placeholder by default; graphics opt-in. Multiplexer support follows Image cards below. |
| Thinking | Collapsed one-liner; expanded 10 dim rows cap. |
| Notice | `note: {text}`, at most 3, never modal, journaled. |
| Status line | Always painted; segments drop whole from the right. |
| Composer | `ratatui-textarea`; `/` and `$` completion lanes. |
| Dialogs and pickers | Take composer+hint rows; reverse-video `>` focus; Esc closes. |

### Markdown and prose

Assistant prose renders from the markdown AST directly to terminal text with tables,
code blocks, inline formatting, and configurable styles:

- Headings render bold. Inline code renders in the `accent` role. Links render with
  underline (OSC 8). Bold text renders with bold emphasis. No italics anywhere. Prose
  honors the measure cap (`Space`).
- Fenced code blocks render full width (no measure cap) inside a frame with a faint
  ASCII `|` left rule; the fence info string renders as a `dim` first row. Syntax
  highlighting is disabled.
- Tables render as ASCII columns (`-`, `|`, `+` only) with cells truncated by measured
  width using ASCII `...`; the header row renders bold; source alignment directives
  are honored so numeric cells right-align; the full table is preserved in the
  expanded view.
- Lists render with `-`; checkboxes render as `[x]` and `[ ]` (ASCII characters, never
  glyphs).
- Completed fenced blocks whose info string is `d2`, `nomnoml`, `dot`, or `mermaid`
  render as diagrams. The block settles only once the art or the fallback is ready.

### Tool cards and execution

The card header displays the spinner (or settled status word), tool name, one-line
argument summary, outcome summary, and duration (omitted when un-timed). Settled headers
are exactly `ok  {name} {summary} · {dur}` (`tool.ok`) or `failed  {name} · {reason} · {dur}`
(`tool.failed`).

- `read`: Echo rows render as `<n>:<text>` with the tool tag footer (`[<n> lines. tag <T>]`,
  `[1 line. tag <T>]`, `[empty file. tag <T>]`; hashline style opens with `[path#TAG]`).
  `letter://` and `dal://` pages render as numbered text. Image results render image
  placeholder cards.
- `search`: Grouped by file; each row is `<n>:<text>` with context rows as `<n>-<text>`;
  hashline style prints `[path#TAG]` headers. The footer counts matches and indicates
  when output was truncated. `symbol` matches render definitions with tags.
- `patch`: Consequence line `patch {path} · +{add} -{del}`, followed by the unified diff.
  Hashline style prints `[path#NEWTAG]` headers from tool echo. Echo rows (`<n>:<text>`,
  hunk separators `--`) render inside the expanded card.
- `exec`: Running state shows the header plus a live tail of the last 12 output lines
  from a ring buffer (full stream written to a blob). Settled state shows one collapsed
  row with exit code, summary, and duration; expanding (`ctrl+o`) shows up to 12 rows
  then `... {n} more lines in the full output` and names the blob path. A background
  exec ending during another turn posts one notice: `exec {id} "{label}": exit {code} in {duration}`
  then its last 10 lines or `no output`. A cancelled job reads `exec {id} "{label}": cancelled`.
- `eval` and plugin tools render with the same card shape; name and one-line summary
  come from the tool result.
- Card expansion is per card, most-recent first with `ctrl+o` inline, or any card in the
  fullscreen transcript view or inline transcript overlay.

### Aggregation rows

Header counts line: `agents  {running} running · {failed} failed · {done} done · eta {eta}`
for child sessions; `jobs  {n} jobs total · {running} running · {done} done` for
background jobs; dalgona run header: `run {label} · {running} running · {queued} queued · {failed} failed · {done} done`.

Row order is binding: failed rows first and never folded away; then running rows in
dispatch order; then fastest finished. A settled row never moves again. Fold line:
`({n} more: {breakdown})` with per-status counts, or `({n} more running)`. Caps: at most
8 rows inline, 25 in the expanded view. Terminal width under 60 columns shows name and
status word only, moving failure reasons to the expand view.

Task line state words: `done`, `blocked`, `failed`, `cancelled`, `lost`. Job rows show
kind, label, state word, and duration.

Wake rule notice (when run completion messages arrive after 20 idle turns): `dalgon
stopped starting turns for finished jobs after 20 in a row. Your next message delivers
the rest.`

### Diff view

Unified diff, full width, no measure cap:

- Roles: `+++` and `---` file headers in `accent` role; `+` rows in `success` role;
  `-` rows in `error` role; `@@` hunk headers in `dim` role; context rows in default
  `text` role.
- Truncation: at most 20 rows inline per file, then `... {n} more hunks`; the full diff
  is preserved in the expand view.
- Responsive collapse: at 60–79 columns, context between hunks collapses to one `...`
  line per gap; below 60 columns, the card displays only the header plus `+a -d` counts,
  with the diff held in the expand view.
- Under `NO_COLOR`, diff signs (`+`, `-`) and the `ok` word carry state. Diff bytes come
  from the tool result; the front end never recomputes diffs.

### Diagrams

Native diagram support covers four kinds: `d2`, `nomnoml`, `dot`, and `mermaid` (D2
preferred; all four accepted). Rendering is CPU or child-process worker work and never
runs on the paint thread; budget is 2 s per diagram:

- Live region shows `diagram {kind} · rendering...` until settled.
- Settled success: text art plus header `diagram {kind} · {n} rows · ctrl+o expands`.
  Box-drawing glyphs measure 1 cell each and the art never exceeds the prose measure
  cap. Art roles: border `dim`, text `text`, edge `accent`, edge label `dim`, title
  `accent` and `bold`.
- Settled failure or timeout: fallback row `diagram {kind}: render failed ({reason}) · source shown`,
  followed by the source text as a standard code block. Never renders a blank block or
  frozen spinner.
- Expand: `ctrl+o` on a diagram card expands to the source text; source is preserved in
  the journal and transcript copy path.
- Terminal fallback: Box-drawing glyphs are East Asian Ambiguous; on terminals
  misaligning art in CJK width mode (where glyphs render 2 cells wide), the source
  view is the honest fallback.

### Images

- Default: placeholder card `image {name} · {w}x{h} · {size}` with expanded note `The
  image was sent to the model. It is not shown in the terminal.`.
- Opt-in `images = true` setting: Kitty or Sixel graphics paint inline when probed OK;
  Kitty takes precedence when both probe OK.
- Under multiplexers (`TMUX`, `STY`, `ZELLIJ`), admit only Kitty placeholders when
  images are enabled, the terminal identifies as Kitty or Ghostty, and the Kitty
  probe succeeds. Keep direct Kitty and Sixel disabled under multiplexers.
- Frozen-bytes invariant: the image paints once; scrollback retains the placeholder card
  text.
- History images (`letter://history/<n>.<i>`) render with the same card shape naming the
  letter ID; with `images = false` the TUI never draws the PNG.

### Thinking

- Collapsed: one row `Thinking {dur} · ctrl+o to show reasoning`.
- Expanded: header `Thinking · {n} steps · {dur} · ctrl+o to hide`, followed by up to
  10 rows of reasoning text in the `dim` role, then `... {n} more steps`. Full text is
  preserved in the expand view.
- Placement: reasoning is never interleaved with the assistant answer; it renders
  directly above the assistant block it belongs to.

### Errors, rule notices, and guard notices

- Errors: name what happened and the next action, render in the `error` role, and
  provide verb-first buttons (`Retry now`, `Cancel turn`, `Show details`).
- Rule fires: render exactly once per fire: `rule {name} fired. The {subject} matched /{pattern}/.`,
  appended with `The turn retries with the rule applied.` for interrupt actions, or
  `The model sees the rule with its next request.` for remind actions. Report fires
  render nothing.
- On interrupt: the TUI removes unsettled live rows of the interrupted response; the
  retried response replaces them.
- Guard notices: the dalgona guard shares the rule-fire format with a `patch` subject;
  a blocked call never renders a tool card because the call never executed.

### Notices

- One row above the activity region: `note: {text}` (for extension `Ui.notify` and system
  notices).
- Guarantees: at most 3 notices, oldest drops first, each carries a word prefix, none is
  modal. Every notice is written to the journal for print mode and JSON subscribers.

### Status line and composer

- Status line: one row, always painted, never truncated on the left. Slots left to
  right: spinner + state word; model ID; path with `~` contraction plus `(branch)`;
  `in {in} out {out}` tokens; `ctx {pct}%`; `agent|s` or `job|s` count; cost.
- Numeric slots are fixed width and right aligned; a value change rewrites its own
  cells and never shifts neighbors. Segments drop whole from the right. The spinner and
  state word never truncate.
- State words: `thinking`, `working`, `fetching`, `compacting`, `retrying`. Idle
  paints no state word.
- Context thresholds: `ctx` takes the `warning` role above 70% and the `error` role
  above 90%.
- Composer: `ratatui-textarea`; prompt row never drops. Completion lanes: `/` lists
  commands with `skill:*` filtered out; `$` opens a skills-only lane with identical
  match and ranking; `$<name>` or `$<ns>:<name>` submits as a skill invocation
  identical to `/skill:<name>`.

## Keys

One key map for both modes; conflicts resolve by ownership order: overlay dialogs, then
app, then composer, then editor. The legacy column (no kitty protocol) is the contract
for a non-kitty terminal; never leave a binding kitty-only.

| Context | Action | Default | Legacy fallback |
|---|---|---|---|
| app | Close overlay, else interrupt turn | Esc | ESC |
| app | Copy selection (when active) | Ctrl+C | 0x03 |
| app | Interrupt turn (no selection) | Ctrl+C | 0x03 |
| app | Quit (composer empty) | Ctrl+D | 0x04 |
| app | Clear live region | Ctrl+L | 0x0c |
| app | Expand or collapse newest card | Ctrl+O | 0x0f |
| app | Transcript overlay (inline) | Ctrl+T | `CSI 20;5~` or none |
| app | Search in transcript (fullscreen) | Ctrl+Shift+F | F3 |
| app | Previous / next prompt | Ctrl+Up / Ctrl+Down | `CSI 1;5A` / `1;5B` |
| app | External editor | Ctrl+G | 0x07 |
| app | Help overlay | F1 | `ESC O P` |
| app | Suspend | Ctrl+Z | 0x1a (POSIX reserved, unbound on Windows) |
| composer | Submit | Enter | CR |
| composer | Newline | Shift+Enter, Ctrl+J | Ctrl+J only (Shift+Enter needs kitty `CSI 13;2u`) |
| composer | Autocomplete accept / cycle | Tab, Shift+Tab | HT (Shift+Tab needs kitty) |
| composer | History previous / next | Up / Down (boundary-aware), Ctrl+P / Ctrl+N | `CSI A` / `B`, 0x10 / 0x0e |
| composer | History search | Ctrl+R | 0x12 |
| editor | Cursor left / right | Left / Right, Ctrl+B / Ctrl+F | `CSI D` / `C`, 0x02 / 0x06 |
| editor | Word left / right | Alt+Left / Alt+Right, Alt+B / Alt+F | `ESC b` / `ESC f` |
| editor | Line start / end | Home / End, Ctrl+A / Ctrl+E | `CSI H` / `F`, 0x01 / 0x05 |
| editor | Delete word back / forward | Ctrl+W, Alt+Backspace / Alt+D | 0x17, `ESC DEL` / `ESC d` |
| editor | Kill to start / end of line | Ctrl+U / Ctrl+K | 0x15 / 0x0b |
| editor | Yank | Ctrl+Y | 0x19 |
| editor | Undo | Ctrl+_, Ctrl+- | 0x1f / none |
| picker | Move | Up / Down, Ctrl+P / Ctrl+N, K / J | `CSI A` / `B` (letters guarded: no filter text) |
| picker | Page / jump | PageUp / PageDown / Home / End, Ctrl+B / Ctrl+F | `CSI 5~` / `6~` / `H` / `F` |
| picker | Accept / cancel | Enter / Esc | CR / ESC |
| picker | Delete row (resume picker) | D | plain d (guarded like J / K) |
| dialog | Move between buttons | Left / Right / Tab | `CSI D` / `C` / HT |
| dialog | Activate button | Enter | CR |
| approval | Allow once | Y | plain y |
| approval | Allow for this session | A | plain a |
| approval | Deny | N, Esc | plain n / ESC |
| approval | View full command / preview | V | plain v |
| retry | Retry now / cancel | R / C, Esc | plain r / c |
| transcript | Page up / down | PageUp / PageDown, Ctrl+B / Ctrl+F | `CSI 5~` / `6~`, 0x02 / 0x06 |
| transcript | Jump to latest | End | `CSI F` |
| transcript (fullscreen) | Extend selection left | Shift+Left | Shift+Left |
| transcript (fullscreen) | Extend selection right | Shift+Right | Shift+Right |
| transcript (fullscreen) | Extend selection up | Shift+Up | Shift+Up |
| transcript (fullscreen) | Extend selection down | Shift+Down | Shift+Down |

### Conflict resolution

- Esc closes an open overlay first; with no overlay it interrupts a running turn; it
  never quits. Quit is Ctrl+D or `/quit` only.
- Ctrl+C copies an active selection first, otherwise interrupts; it never exits dal.
- Up and Down mean history recall at the composer boundary, cursor movement inside
  multi-line input, and picker rows when a picker holds focus; the hint line names the
  owning context.
- Tab accepts autocomplete when the popup is open, else inserts two spaces; it never
  leaves the composer.
- Ctrl+B and Ctrl+F are cursor movement in the composer, and paging in pickers and the
  transcript; context determines ownership.
- Ctrl+R is history search only.
- Plain-letter actions fire only where no text field owns input.

### Focus and overlays

- Dialogs trap focus and return it to the composer on close.
- A dialog starts focus on the conservative verb: `Keep editing` for exit, `Deny` for
  approval; single-letter verbs stay active.
- Dialog boundary: one blank row above and below the dialog, title row bold, and focus
  rows in reverse video plus the `>` marker.
- The fullscreen transcript takes focus only when entered (Ctrl+T overlay in inline;
  PageUp or Ctrl+Shift+F in fullscreen); typing anywhere returns focus to the
  composer.
- Keyboard reach: every overlay is reachable and dismissible by keyboard; no mouse-only
  interaction exists: every click target has a key, every drag action a chord
  (Shift+arrows extend selection, mouse wheel scrolling equals PageUp and PageDown).

### Consent lock

A dialog never takes consent from type-ahead. Each request dialog starts locked: for
500 ms after its first frame it drops every key but an unmodified Esc, which only denies
or dismisses. The hint row reads `esc denies · answer keys ready in a moment` while
locked and lists the answer keys (`y allow · a session · n deny · v view · esc denies`
for an approval or grant) only once they act, so a user or a gate that sees the keys can
press one. Dropped keys are never queued. A request that follows another starts locked
again. Only a key pressed after the answer keys are on screen can approve.

### Keyboard protocol and terminal fallbacks

- Kitty keyboard protocol: enabled with `CSI > 1 u` (disambiguate only) on inline
  startup and on entering the alternate screen; popped with `CSI < u`; at process exit
  emit the stronger reset so a crash never leaves the parent shell enhanced.
- Decode kitty and legacy grammars simultaneously; query `CSI ? u` once with a 300 ms
  timeout; under tmux read the `extended-keys` option before pushing flags.
- Degradation: Shift+Enter falls back to Ctrl+J and the hint shows the working key;
  Ctrl+Shift+F falls back to F3; shifted chords with no legacy form are unbound and
  absent from hints.
- Windows Terminal: crossterm console support (raw mode via `enable_raw_mode`, VT
  sequences on Windows Terminal and ConPTY). The kitty protocol is not available
  there: the legacy column is the contract for every chord. OSC 52 writes work; when
  refused, the notice `copy failed: the terminal refused the selection` appears.
  Bracketed paste is honored and a bare end marker (`ESC[201~`) tolerated. Suspend
  (Ctrl+Z) is a POSIX no-op on Windows and unbound there.

### Startup validation

Startup validation rejects printable-key bindings with the exact string:
`binding {context}.{action} uses a printable key. Printable keys are reserved for typing.`

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

1. Every role, width, row count, and truncation above is satisfied, or its deviation is
   documented in this file.
2. The same state reads correctly under `NO_COLOR`, `DAL_NO_MOTION`, 16-color, and
   20-column width.
3. The contrast gate still passes for every shipped theme.
4. The frame-level invariants F1-F8 in `CONCEPTS.md` still hold on the changed path.
