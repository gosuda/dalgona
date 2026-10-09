# quality

The quality battery watches streamed model output with three report-only
detector lanes: collapse-repetition, control-token-leak, and repetitive-
turns. Their fires land as durable rule_fired records and stay silent in
human views. It also registers the text rule
fabricated-unavailable-tool-call in always-interrupt mode; a matching
pattern interrupts the response.

For each turn, the guard reports from its retained measurements; the
quality battery reads that same per-turn findings snapshot for offers and
does not measure those files again:

- turn growth: added, deleted, and net lines and the files touched.
- per-file absolute metrics: up to 20 displayed files per patch call get a receipt
  with code lines, function count, and cognitive and cyclomatic sums; any
  remaining files are summarized by count.
- per-function metric changes: each band crossing is one line with the
  function and its before-to-after cognitive, cyclomatic, size, or nesting
  value; file-size crossings are labeled by path. The turn summary ranks
  crossings and keeps the top ten.
- best-current: when erosion rises by the configured threshold, `METRICS`
  gives the files, bands crossed, and erosion before/after. The report then
  ranks per-function current-mass changes by greatest increase, filling the
  remaining rows of the ten-row cap after band crossings.

Guard findings become codemod offers: delete-commented-code (not for Rust)
and rethrow-empty-catch spans that cover whole lines. Offers land as
durable quality_offers records, the model sees one notice that lists them,
and quality_apply applies one offer through the normal preview and
approval path after a byte-for-byte staleness check. A quality offer never
writes a file directly.

Config: the `[plugin.quality]` table accepts no keys; the guard is turned
on through the product's `[guard]` table.
fs.readquality0.1.0codemod_offers
