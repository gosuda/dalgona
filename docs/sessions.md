# Sessions, history, and compaction

Sessions live under the data root. Use `-c` to continue a session. Use `-r` to resume one. Names label sessions. Exit lines and `/name` print the `-r` argument in the exact form the shell accepts. Names that are unsafe to paste degrade to the session id. Use `/tree`, `/fork`, and `/clone` to inspect or change branches. Use `/export` to write a shareable file. Read an entry at `session://<id>/<entry>`. `--no-session` runs without saving.

Child sessions also write a `dal-agent` `child_policy` record with their inherited approval mode and tool allowlist. An absent tool list means unrestricted access. Session replay requires at most one policy record. Duplicate or conflicting records fail replay.

A reopened session shows the model, thinking level, approval mode, execution mode, and name its journal recorded. The last record of each kind wins. A session that never chose a model keeps following the configured default. Its settings name no model while it runs and after it reopens. A default saved after it opened applies the next time it opens. A route the session chose shows exactly as recorded, with its provider when the record names one.


## Durable turn identities

For saved sessions, dal writes a turn-ID reservation to the journal before it acknowledges a prompt, follow-up, or wake allocation. A record counts as saved only after it is synced to disk. The first reservation creates the journal even if the turn never starts.

Private tool effects also require saved receipts. In a saved session, the first private loop or effect receipt creates the journal even if no user turn has started. dal saves the start receipt before running the effect. Ephemeral sessions keep these records in memory and do not create journal files.

Cancelling queued work does not release its identity. Later turns keep their allocated IDs. When a session reopens, unused reservations remain consumed. dal does not restore queued text. Submit it again if you still want to run it. A reservation alone does not create a completed turn.

Reservation records use journal format 2, and mail delivery records use journal format 3. All other record kinds retain format 1, so one journal can contain several versions. Existing journals remain readable and are not rewritten. Once a record of a newer format is written, a build that does not read that format reports the newer format and does not open the journal. Use a build that supports formats 1 to 3.

Multi-record batches still lack atomic framing against process crashes. An interrupted batch is not guaranteed to roll back cleanly on reopen.


## Compaction

Compaction starts by default at 85 percent of the context window, remote first. Use `/compact <focus>` to force it. Compaction retains recent entries and inserts summary text. dal drops raw provider-native history items. They do not continue into subsequent model requests.

## Context measurements

Measured context use applies only to the model route that produced the latest
assistant response on the selected branch after compaction. The route includes
its kind and, for API routes, its API family. A matching model name alone is not
enough. When both routes name a provider, the providers must match too. A
response recorded without a provider still counts for the same model that now
names one.

Completed and partial responses retain this identity when the session reopens.
If the latest response has no route metadata, dal uses a context estimate rather
than an older measurement. Existing journals stay readable and are not rewritten.
See [assistant entries](protocol.md#assistant-entries) for the stored field.

## Next

- [Slash commands](commands.md): run `/tree`, `/fork`, `/clone`, `/export`, `/compact`, and `/resume`.
- [Command line](cli.md): continue or resume a session with `-c` and `-r`.
- [Protocol](protocol.md): list, open, and subscribe to sessions over JSON-RPC.
