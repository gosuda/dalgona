# CONCEPTS: the terminal interface model

## Letterpress

Every line is set once and stays true. Two surfaces exist:

- The committed transcript: terminal scrollback. Write-once, never repainted. dal never
  reflows what it already set. The live block: the only repainted surface. Differential,
  measurable, settles by committing its full rendered height into scrollback and
  shrinking.

There is no third surface. The live block is the single accent; the transcript holds
flat ground. The interface serves the work: the register is product, chrome is one row
per region, nothing decorative occupies a row.

Rivals rejected: a stage-only TUI breaks scrollback, screen readers, and the 1 s startup
budget; a pure log streamer cannot host dialogs, pickers, or approvals.

## Modes

- Inline mode (default): dal owns only the live block; scrollback holds the transcript
  byte-for-byte as it streamed. No mouse capture. Fullscreen mode (`screen =
  "fullscreen"`): the same grammar on the alternate screen. On exit, the settled
  transcript replays into scrollback so the record stays complete; on unclean exit, one
  scrollback marker names where the transcript lives.

## Regions

The live block stacks bottom-anchored in fixed order: notices, activity, composer, hint,
status. Status always paints; the composer prompt row never drops. Overlays (dialogs,
pickers) take the composer and hint rows; activity yields first; one dialog at a time.

## Data flow

All components render from `Update` values and structured `View` data only; nothing
reads the journal. State flows in one direction: provider and orchestration events
produce updates, updates produce view data, view data produces cells. The renderer is a
pure function of that data plus terminal dimensions.

## Frame model

A frame is the ratatui buffer diff plus cursor motion, wrapped in
`BeginSynchronizedUpdate`/`EndSynchronizedUpdate` when DECRPM 2026 answers, exactly one
pair per frame, zero nesting. Dirty flags are per region; a frame renders only dirty
regions and dependents. Streaming deltas coalesce at 33 ms, capped at 60 Hz; a key event
requests an immediate frame that bypasses the batch. Height changes (commit, grow,
shrink) are single synchronized updates containing a full region repaint.

## The flicker-free contract

| # | Rule | Check |
|---|---|---|
| F1 | No full-screen clear; no erase-display ever | PTY byte assert |
| F2 | One sync-bracket pair per frame, zero nesting | recorder balance |
| F3 | Constant-height frames write only changed cells | golden bytes 80x24 |
| F4 | Committed rows are write-once | scrollback tail equals golden transcript |
| F5 | Height changes are atomic single updates | frame-byte caps |
| F6 | SGR resets; matched mode pairs; crash restores shell | exit fixtures |
| F7 | 16 ms interactive, 33 ms coalesced, ≤ 60 Hz; repaint ≤ 4 ms CPU | budget harness |
| F8 | First frame ≤ 1 s; init ≤ 50 ms; no paint block | cold-start harness |

Ratatui's stock inline viewport is not used: its resize duplicates content into scrollback (ratatui
issues #2086, #2666, #984). The region layer owns row arithmetic; ratatui diffs cells
only.

## Honest degradation

Every capability degrades along a named ladder rather than failing silently: truecolor
to 256 to 16-color to `NO_COLOR`; motion to `DAL_NO_MOTION`; kitty keys to legacy
modifiers; inline images to placeholder cards; kitty/sixel images to text. A state that
cannot render as designed renders as words and markers — meaning never depends on a
channel the terminal did not confirm.
