# quality

The quality battery watches streamed model output with three report-only
detector lanes: collapse-repetition, control-token-leak, and repetitive-
turns. Their fires land as durable rule_fired records and stay silent in
human views. Repetition collapse is reported as control-token-leak when
the control-token detector confirms a correlated token run; otherwise it
stays collapse-repetition. Repetitive-turn detection reports once at the
third similar turn. An eligible dissimilar turn resets the streak.
Each turn retains up to 32 detector fires. Further fires set
`{"truncated":true}` in the turn-end record. Stored text excerpts are
capped at 64 KiB; that text cap does not set the truncated flag.

The lanes hold per session: `rules.watch = false` holds every lane and
prints the rules watch note; a lane name in `[rules] disabled` holds
that lane; and a user rule file named after a lane (for example
`collapse-repetition.md` under `<data-root>/rules/`) replaces the
compiled lane, which stands down while the user rule watches instead.

It also registers the text rule
fabricated-unavailable-tool-call in always-interrupt mode; a matching
pattern interrupts the response.

For each turn, the guard reports from its retained measurements; the
quality battery reads that same per-turn findings snapshot for offers and
does not measure those files again:

- `TURN CHANGE SUMMARY.`: added, deleted, and net lines with the files touched.
- `guard:` receipts: up to 20 displayed files per patch call get a receipt
  with code lines, function count, and cognitive and cyclomatic sums; any
  remaining files are summarized by count.
- band crossings: each crossing is one line with the
  function and its before-to-after cognitive, cyclomatic, size, or nesting
  value; file-size crossings are labeled by path. The turn summary ranks
  crossings and keeps the top ten.
- `METRICS:`: when erosion rises by the configured threshold, the `METRICS:`
  line gives the files, bands crossed, and erosion before/after. The report then
  ranks per-function current-mass changes by greatest increase, filling the
  remaining rows of the ten-row cap after band crossings.

Guard findings become codemod offers: delete-commented-code (not for Rust)
and rethrow-empty-catch spans that cover whole lines. Offers land as
durable quality_offers records, the model sees one notice that lists them,
and quality_apply applies one offer through the normal preview and
approval path after a byte-for-byte staleness check. A quality offer never
writes a file directly.

The `[plugin.quality]` table accepts no keys. Unknown keys and a non-table
value fail startup. The four detector rules belong to the compiled
`detectors` rule set, enabled by default through `[rule_sets].enabled`.
Omitting `detectors` from that list disables the three streaming detector
lanes and fabricated-unavailable-tool-call. It does not remove
`quality_apply` or guard metrics. Configure the guard through `[guard]`.
