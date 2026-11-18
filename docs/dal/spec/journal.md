# Event journal

The journal MUST survive a process halt, a forced kill, and a host restart. JSONL is the authority. The mirror is `state.db`. It is repairable from JSONL.
Directory `fsync` MUST be a no-op on Windows because the operating system rejects directory sync.

## Record format

Each thread uses `sessions/<id>/journal.jsonl`. Each line MUST contain one object followed by `\n`:

```json
{"seq":1,"prev":"<64 hex>","hash":"<64 hex>","crc":"<8 hex>","ev":{}}
```

`ev` MUST be one JSON object with a string `type` field. The journal MUST preserve its raw bytes. `crc` MUST be CRC32-Castagnoli of the raw event bytes. `hash` MUST be SHA-256 of the 64 ASCII bytes of `prev`, the big-endian sequence number, and the raw event bytes. Sequence numbers MUST start at one and increase by one. The first `prev` value MUST be 64 zeroes. A line, including its trailing newline, MUST NOT exceed 16 MiB.

A payload larger than 64 KiB MUST be stored under `blobs/<sha256>`. The event MUST contain the SHA-256 name and byte count. Blob writes MUST use a temporary file, file `fsync`, rename, and directory `fsync`.

## Append

`Append` MUST validate every event before writing. It MUST encode all events for one call and write them with one append write. It MUST `fsync` the journal before it returns. It MUST write the same records and thread head in one synced mirror batch. JSONL remains authoritative when the mirror write fails. The store MUST retain the new JSONL head and report the mirror error.

On Linux, when `DALGONA_SYNCFS` is set, the journal `fsync` MUST be followed by `Syncfs` for that directory. On other systems the variable MUST have no effect.

## Open and repair

`Open` MUST verify JSONL framing, JSON shape, sequence numbers, CRC values, and the hash chain. The quarantine `<offset>` MUST be the byte offset of the first bad line. The following rules MUST apply.

| Finding | Required action |
| --- | --- |
| The final line has no newline, cannot be parsed, or has a CRC or hash failure | Copy the partial final record to `quarantine/<session>-<offset>.bin`. When that file exists, the store MUST write `quarantine/<session>-<offset>-<seq>.bin` where `<seq>` is a per-process sequence number. The store MUST NOT overwrite a file in quarantine. Truncate at the last good line. `fsync` the file and directory. Log `journal: <session>: torn tail of <n> bytes quarantined`. Apply the verified mirror-suffix rule afterward. |
| A record has a sequence gap, a duplicate sequence, or a bad link at any position, or another bad record before the final line | Use the mirror only when every row from the damage point through the mirror head verifies against the chain. Rebuild JSONL from the good prefix and those rows through a temporary file, rename, and directory `fsync`. Keep every later JSONL record that verifies against the resulting chain. Rebuild the mirror from the kept JSONL. Valid records past the mirror head MUST NOT be discarded. |
| The mirror cannot supply that complete verified suffix | Mark the thread damaged at the first bad sequence. Keep only the valid prefix for replay. Reject appends. Preserve the bad suffix in quarantine before writing the damage marker. Report `journal: <session>: damaged at seq <n>; quarantined at quarantine/<file>`. |
| `state.db` cannot be opened, or a mirrored hash differs from JSONL | Move `state.db` to `quarantine/state-<nanos>-<pid>-<seq>.db` (and `state.db-wal`, `state.db-shm` to the same stem with those suffixes when present). `<nanos>` is Unix time in nanoseconds, `<pid>` is the process id, and `<seq>` is a per-process sequence number. All paths of one quarantine MUST share one stem. When a candidate name exists, the store MUST use the next free name with `-<n>` added to the stem. The store MUST NOT overwrite a file in quarantine. Rebuild the mirror from JSONL. |
| The mirror is behind JSONL | Replace the mirror rows of the thread with the JSONL rows. |
| The mirror is ahead of a complete JSONL file | Append the mirror suffix only when every row verifies from the JSONL head. Otherwise rebuild the mirror from JSONL. |

`Replay` MUST return every valid prefix event before it returns a damage error. A damaged thread MUST remain read-only across later opens. The store MUST NOT silently discard a committed event.

For a damaged thread, `Append` MUST fail with `journal: session <id> is damaged at seq <n>`. When the thread journal file is absent and the thread has committed events, `Replay` MUST fail with `journal: session <id> journal missing`.

## Pebble mirror

The mirror is the Pebble directory `state.db`. The backup is `state.db.bak`. Every committed write MUST sync its batch. The store MUST hold `state.db.lock` before opening `state.db`, waiting up to 5000 ms before it fails an operation. An exclusive store holds that lock for its lifetime. A shared store holds it only across one open, write, or read batch.

The mirror maps `journal.jsonl` onto these keys:

```text
"v"                                     -> mirror schema version "1"
"s" + len16(id) + id                    -> status \x1f head_seq \x1f updated_at
"e" + len16(sid) + sid + seqBE8         -> hash \x00 ev
"i" + len16(sid) + sid + seqBE8         -> kind \x00 body
"t" + trigram + \x1f + len16(sid) + sid + seqBE8 -> ""
```

`status` is `ok` or `damaged`, `head_seq` is a non-negative integer, and `updated_at` is RFC 3339 UTC. Loads MUST coerce a bad `status` to `damaged` and a missing or negative `head_seq` to 0. An upsert MUST keep a prior `damaged` status.

Every mirror write MUST add the index rows of the session inside the same batch as its event rows. A session replacement MUST delete the index rows of the session first. A rebuilt mirror MUST rebuild the index from JSONL, because the index is derived data.

The index holds the inline `text` of `user.message` and `assistant.message` events and the `name` of `thread.named` events. `Search` MUST index no other event and no text that lives in a blob. `Search` MUST reject a query whose term holds fewer than 3 runes. `Search` MUST rank sessions by their best matching row and MUST cap the result at 1000 sessions.

The index stores the lowercase rune trigrams of each indexed body as posting keys. `Search` MUST intersect the posting sets of every term trigram. It MUST verify each term as a case-insensitive substring of a candidate body before it returns a hit.

`Search` MUST omit a session whose first event is `session.created` with `ephemeral` set to `true`. The index still holds the rows of that session. `Search` MUST keep every other session. That includes a session whose `session.created` event has `ephemeral` absent or `false`. It also includes a session whose first event has another type and sets `ephemeral`. A positive `Limit` below 1000 MUST lower the cap. `Search` MUST apply the cap and the limit after it omits ephemeral sessions. An omitted session never uses a slot of the result.

The mirror MUST remove the rows of one session with one batch. That batch deletes the posting keys, the index rows, the event rows, and the session row of the session. It MUST NOT change the JSONL file, which stays append-only. A removal of a session with no rows MUST succeed and MUST NOT change another session.

The mirror MUST record its schema version in the `v` key. The store MUST reject a mirror whose version exceeds the supported version. A mirror without the key adopts version 1 with no row changes.

Every 30 minutes and at clean close, the store MUST checkpoint `state.db` into a private staged directory `state.db.bak.tmp.staged.<pid>.<seq>`. It MUST then rename the staged directory to `state.db.bak.tmp`, rename `state.db.bak` to `state.db.bak.old`, and rename `state.db.bak.tmp` to `state.db.bak`. It MUST retire `state.db.bak.old` to a `state.db.bak.tmp.trash.<pid>.<seq>` name under the same lock. The store MUST hold an exclusive lock on `repair.lock` from stale-path retirement through the renames. The store MUST remove retired `state.db.bak.tmp.*` directories after releasing `repair.lock`. Within one process, a checkpoint staged later MUST install after an earlier one. A clean close MUST report a backup failure. When `state.db` is unusable, the store MUST try `state.db.bak`, then `state.db.bak.old`, then each `state.db.bak.tmp` and `state.db.bak.tmp.staged.<pid>.<seq>` directory from newest to oldest, until one opens. The store MUST NOT treat a `state.db.bak.tmp.trash.<pid>.<seq>` directory as a backup. This rule allows a verified backup row to repair damaged JSONL.

## Locks

`Open` MUST hold an exclusive process lock on `daemon.lock`. A second owner MUST receive `another dalgona process owns <home>`. `Open` repairs every thread and can replace `state.db`. A home that `Open` uses MUST NOT have `OpenShared` users.

`LockSession` MUST hold an exclusive lock at `sessions/<id>/lock`. It MUST take that lock while it holds an exclusive lock on `sessions/<id>/repair.lock`, and it MUST wait for that gate. A second owner MUST receive `another dal process owns session <id>`. The caller MUST call the returned unlock function.

## Shared open

`OpenShared` lets many processes use one home. It MUST NOT lock `daemon.lock`. It relies on thread locks for journal writes. The caller MUST hold `LockSession` for a thread before it calls `Append` for that thread.

- Mirror join: `OpenShared` MUST hold an exclusive lock on `repair.lock` while it opens `state.db`. Under that lock it MUST try an exclusive lock on `users.lock` without a wait. When it gets the lock, no other shared store is open. Only then MAY it apply the `state.db` quarantine and backup rules. It MUST then release the exclusive lock and take a shared lock on `users.lock` before it releases `repair.lock`. It MUST hold the shared lock until close. When another shared store is open and `state.db` is unusable, `OpenShared` MUST fail and MUST NOT move `state.db`.
- Thread repair: the store MUST apply the repair table to a thread only while a thread lock is held. At open and before `Replay` reads a thread, the store MUST check journal freshness. It MUST compare the file identity, size, and modification time with its cached values. When those values match, it MUST NOT probe the thread lock or reload the thread. Otherwise, it MUST hold `sessions/<id>/repair.lock` during the reload or lock probe. When the calling Store holds the thread lock, it MUST skip the lock probe. It MUST then reload the thread directly. Otherwise, it MUST try `sessions/<id>/lock` without a wait. It MUST reload and repair the thread when it gets that lock. It MUST skip the repair when another owner holds the lock. A damaged thread then stays read-only. `Append` MUST trust the caller's thread lock.
- Mirror repair: a mirror hash mismatch or an unverifiable mirror suffix MUST replace the rows of that thread from JSONL. The store MUST NOT quarantine or rebuild the whole `state.db` in shared mode.
- Freshness: before `Append`, the store MUST compare the journal file identity, size, and modification time with the values after its last load or write. On a difference, it MUST reload the thread through the repair table. Before the reload repairs the thread file, the store MUST close its own append handle for that thread. `Sessions` MUST reload the thread rows from `state.db`.
- Replay: `Replay` MUST stop without error at a final line that has no `\n`. Another owner can be in the middle of that append.

In both modes, the store MUST keep the mirror rows of a thread when they hold exactly the JSONL sequence numbers and hashes. It MUST then update only the thread row.

## Compaction

A compaction marker MUST keep each tool result with the assistant message that issued it. The scope of a call id or a run id is the part before "/". A cut MUST be safe only when no scope occurs on both sides of the cut. A cut MUST start at a user message or an assistant message.

The agent MUST NOT append a compaction whose `tokens_after` is not smaller than `tokens_before`. An automatic compaction that does not shrink MUST leave the turn running without a marker. After an automatic compaction that does not shrink, the agent MUST NOT run another automatic compaction until a compaction marker is appended. A manual or overflow compaction MUST fail with `compaction: the summary is not smaller than the context it replaces (<before> tokens before, <after> after)`.

## Blobs

Blob names MUST contain exactly 64 lowercase SHA-256 hexadecimal characters. `Blob` MUST reject malformed names and report missing blobs.

An image MUST be stored as a blob, whatever its size.
