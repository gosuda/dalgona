# history

The history battery registers one compactor named `history` and one automatic
consolidation task for captured history letters.

## Configuration

| key | default | range |
|---|---|---|
| enabled | true | true or false |
| share | 0.4 | 0.1 to 0.7, never clamped |

`share` is the fraction of the context window that history images may bill. A
value outside the range, a non-number, a non-boolean `enabled`, or an unknown
key under `[plugin.history]` prints one startup warning. The `history`
compactor then refuses every compaction with that same text, and no
consolidation task runs. `enabled = false` registers neither the compactor nor
the task.

## Compaction

The chain order is `remote`, then `history`, then `summary`. History reads the
catalog image profile supplied for the resolved model; it never infers image
support or billing from a model name. Without a profile it refuses with
`history: the model does not read images.` When the profile is present but the
atomic image commit service is unavailable, history refuses with `history:
this host cannot commit image parts.` and the local text summary runs. No
image is emitted without durable source text readable through `letter://`.

History needs at least two user turns in the covered span. Consecutive user
entries belong to one open turn. A smaller span refuses with
`history: no new history to draw.` and passes to the next compactor.

History also refuses when the source text is unavailable, the context window
is unknown, the images do not save enough tokens, or the token, image-count,
or PNG-byte budget is exceeded. The refusal names the limit.
A missing stored text blob reports `history: the journal source is unavailable.`;
a filesystem error reports the underlying service error instead.
Drawing and chain timeouts stop the image attempt. The next registered
compactor can then provide a text summary.

When history commits, the compaction carries image parts interleaved with
their `letter://<id>` labels, one final index text, and one `letter` record
per fresh image; each PNG is stored once under its digest. A later
compaction that re-draws the same journal under the same grid reuses the
stored letters: their PNG bytes, ids, and digests come back unchanged. A stored
letter whose cell pitch differs or whose image exceeds the current grid is
stale; it stays listed and resolvable, and its content is drawn again under
a fresh id.

Before rendering, history indexes prior compaction metadata without loading
their PNG blobs. It fetches a blob only for a matching reusable span after
acquiring the shared render budget; a missing or damaged blob falls back to
redrawing exact source text. Skill captures and dream summaries are not
counted as history images.

## Letters and ids

History record ids use `history/<ordinal>.<index>` for compaction images,
`<name>`, `<name>.2`, `<name>.3` for skill captures, and `dream/<ordinal>` for
consolidated summaries. This battery appends dream records and does not edit or
delete existing records. Read a letter with `letter://<id>` or list letters
with `letter://`; ids resolve on the current session path, so a branch may
name different content under the same id.

## Auto-dream

The task counts captured letters that no dream record has consumed yet. At 30
unreflected letters it submits one consolidation job when the 20-minute idle
timer fires, or at the next `settled` event, never while a turn runs. The job
makes one judge call and appends a `dream/<ordinal>` record, then rewrites the
`dream.json` sidecar. Records are the source of truth: a record wins over a
stale sidecar. The task adds no command, tool, or turn wake and never injects a
prompt.

- Judge off: no model call, `Auto-dream skipped: the judge is off.`
- Success: `Auto-dream consolidated <n> letters.`
- Failure: `Auto-dream failed; the batch remains unreflected.`
- Three identical or six transient failures in a row park the task with one
  notice, `Auto-dream paused after repeated failures (3 identical or 6
  transient). It will retry once per 6 hours.`
- While parked, one probe runs per 6 hours. A failed probe reports
  `Auto-dream remains paused; the next probe is in 6 hours.` A successful probe
  resets both counters and unparks.
- Ephemeral sessions have no sidecar, so no job runs and the task reports
  `history: auto-dream is unavailable in an ephemeral session.`
