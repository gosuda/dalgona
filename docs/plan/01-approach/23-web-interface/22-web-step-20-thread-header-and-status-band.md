#### Web step 20. Thread header and status band

- Token usage parsing. `validation.ts` adds case `thread/tokenUsage/updated` → `{ type: "tokenUsage", threadId, turnId, usage: TokenUsage }` with `TokenUsage { last: Breakdown; total: Breakdown; contextWindow: number | null }`.
  - `Breakdown` holds `totalTokens`, `inputTokens`, `cachedInputTokens`, `cacheWriteInputTokens`, `outputTokens` and `reasoningOutputTokens`.
  - Add it to the conformance table with `ThreadTokenUsageUpdatedNotification.json`.
- Other parsing.
  - `ModelOption` gains `efforts: readonly string[]` and `defaultEffort: string | null` (`efforts` holds the `reasoningEffort` of each `supportedReasoningEfforts` entry; `defaultEffort` comes from `defaultReasoningEffort`).
  - `StatusInfo` drops `runner` (delete `runnerLabel` at `validation.ts:180-186`) and gains `effort: string | null` from the thread response member `reasoningEffort` (`internal/rpc/types.go:82`), which `statusInfo` (`validation.ts:441-444`) reads.
  - `TurnSnapshot` gains `startedAtMs: number | null` (from `turn.startedAt` × 1000) .
- Session. Adds `usage: Readonly<Record<string, TokenUsage>>`; `thread/resume` supplies `tokenUsage`. Token counts are not shown; the context percent is.
- Gauge rule. Context used follows the formula of the terminal workstream, Step 12, over the `last` breakdown.
- Components.
  - No `ThreadHeader` component. `App.svelte` renders `<header class="thread-head">` with the `<h2>` title (`overflow-wrap:anywhere`) at the left and the `StatusBand` in a right-aligned margin column (Design record, Status band). The live `StateTag` sits after the title.
  - New `web/src/lib/components/StatusBand.svelte` is a `<dl class="band">` with `container-type: inline-size`. Its items are the Design record, Status band list in this order, each with `data-item="<id>"`. An item whose id is in `statusHide` is not rendered. Layout: three rows at 640px and wider (`model`; `effort approval sandbox folder queue`; `context mark`). Items:
    1. `model`: the model name as text, a button that opens the model panel (step 29; until then the existing `ModelPicker` select). Never hidden.
    2. `context`: the moon gauge (Design record, Icons: 15 Nerd glyphs U+E38D to U+E39B, index `round(f * 14)`, `f` clamped to 0 to 1) and the percent `${Math.round(100 * used / window)}%`. `aria-label` is `Context ${pct}% used`. Without a window the item is hidden, because `modelContextWindow` is optional (`internal/rpc/types.go:226`, `omitzero`).
    3. `effort`: one Nerd glyph by rank `round(rank * 3 / (levels - 1))`: `md-gauge_empty` U+F0873, `md-gauge_low` U+F0875, `md-gauge` U+F029A, `md-gauge_full` U+F0874. `aria-label` is `Effort ${effort}`. Hidden when `efforts` has fewer than two entries (cross-phase decision 15).
    4. `approval`: `md-hand_back_right` U+F0E47 for `ask`, `md-robot_outline` U+F167A for `auto`, `md-fire` U+F0238 for `yolo`. Never hidden by the narrow rule. `aria-label` is `Approval ${value}`.
    5. `sandbox`: `md-package_variant_closed` U+F03D7 for `workspace-write` and `md-alert_outline` U+F002A for `off`. Those are the only two values of the config key `sandbox` (`internal/config/config.go:105-108`). `sandboxLabel` maps the wire value `dangerFullAccess` to `off` (`validation.ts:165-166`), so both take that glyph. `aria-label` is `Sandbox ${value}`.
    6. `folder`: `md-folder_outline` U+F0256. `title` and `aria-label` are the folder name.
    7. `queue`: `md-timer_sand` U+F051F and the count, only when a follow-up is queued. `aria-label` is `${countLabel(n, "follow-up", "follow-ups")} queued`.
    8. `mark`: the small candy mark (`dalgona-small.svg`, Design record, Product marks).
    Not band items: the live state tag (in the header), the advisor (`/advisor`, step 32), and a lost connection (the disconnect notice, step 6).
  - Narrow rule (Design record, Status band): below 640px the margin column becomes one row under the title. Whole items drop in the order `folder`, `queue`, `effort`, `mark`. `model`, `context`, `approval` and `sandbox` never drop; if they still do not fit, `model` is cut from the right with `…`. Unicode glyphs are not drawn here: the browser always uses the Nerd glyphs (Design record, Icons).
  - `<details class="band-more"><summary>Status</summary>` lists every shown item as text, and appears when an item is dropped or hidden, so no value is lost.
- Removed. `StatusBar.svelte` and the `.workspace-heading` block (`+page.svelte:591-602`).
- Document title. `App.svelte` sets `<svelte:head><title>{titleText}</title>`. The title is `dalgona · ${title} · ${stateWord}` in every state, the same order and words as the terminal window title (terminal Step 36).
- Tests.
  - Conformance row for token usage.
  - `StatusBand.dom.test.ts`:
    - `shows effort and context as text`
    - `lists every segment under Status details`
    - `shows the none glyph when the sandbox is off`
    - `hides the items named in statusHide, and never model`
  - Browser check `web/checks/status-band.js` at 320x568: `model`, `context`, `approval` and `sandbox` are visible, and `folder` and `queue` are dropped.
  - `App.dom.test.ts`: `the document title carries the state word`.
  - Fails before: `thread/tokenUsage/updated` has no case in `validation.ts`, so it is ignored. The `serverRequest/resolved` case sits at `validation.ts:767`.
- Spec (`docs/spec/web.md`). Rename `### Status bar` to `### Thread header and status band`: items (Design record, Status band), drop order and `statusHide`, icons (Design record, Icons), the details list, the title rule, and the removal of the runner segment. The sentence that Step 2 adds about the `seq` guard stays under the new heading.
- Lands after: Steps 2, 4, 7 and 10 (`Session`, `countLabel` and `StateTag`).

Anchor: Edward Tufte, Herbert Simon.

