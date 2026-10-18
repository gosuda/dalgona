# Sessions, history, and compaction

Sessions live under the data root. Use `-c` to continue a session or `-r` to resume one. Names label sessions. Use `/tree`, `/fork`, and `/clone` to inspect or change branches. Use `/export` to write a shareable file. Read an entry at `session://<id>/<entry>`. `--no-session` runs without saving.

Child sessions also write a `dal-agent` `child_policy` record with their inherited approval mode and tool allowlist; an absent tool list means unrestricted access. Session replay requires at most one policy record; duplicate or conflicting records fail replay.


## Durable turn identities

For saved sessions, dal writes a turn-ID reservation to the journal before it acknowledges a prompt, follow-up, or wake allocation. A record counts as saved only after it is synced to disk. The first reservation creates the journal even if the turn never starts.

Private tool effects also require saved receipts. In a saved session, the first private loop or effect receipt creates the journal even if no user turn has started. Ephemeral sessions keep these records in memory and do not create journal files.

Cancelling queued work does not release its identity. Later turns keep their allocated IDs. When a session reopens, unused reservations remain consumed. Queued text is not restored, so submit it again if you still want to run it. A reservation alone does not create a completed turn.

Reservation records use journal format 2. All other record kinds retain format 1, so one journal can contain both versions. Existing journals remain readable and are not rewritten. Once a format-2 reservation is written, a build that reads only format 1 cannot open that journal. Use a build that supports both formats.

Multi-record batches still lack atomic framing against process crashes. An interrupted batch is not guaranteed to roll back cleanly on reopen.


## Compaction

Compaction starts by default at 85 percent of the context window, remote first. Use `/compact <focus>` to force it. Compaction retains recent entries and inserts summary text. Raw provider-native history items are dropped and are not continued into subsequent model requests.

## Context measurements

Measured context use applies only to the model route that produced the latest
assistant response on the selected branch after compaction. The route includes
its kind and, for API routes, its API family. A matching model name alone is not
enough.

Completed and partial responses retain this identity when the session reopens.
If the latest response has no route metadata, dal uses a context estimate rather
than an older measurement. Existing journals stay readable and are not rewritten.
See [assistant entries](protocol.md#assistant-entries) for the stored field.
