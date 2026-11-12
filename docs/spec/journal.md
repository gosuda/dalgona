# Event journal

The journal MUST survive a process halt, a forced kill, and a host restart. JSONL is the authority. The mirror is `state.db`. It is repairable from JSONL.
Directory `fsync` MUST be a no-op on Windows because the operating system rejects directory sync.

## Record format

Each thread uses `sessions/<id>/journal.jsonl`. Each line MUST contain one object followed by `\n`:

```json
{"seq":1,"prev":"<64 hex>","hash":"<64 hex>","crc":"<8 hex>","ev":{}}
```

`ev` MUST be one JSON object with a string `type` field. The journal MUST preserve its raw bytes. `crc` MUST be CRC32-Castagnoli of those bytes. `hash` MUST be SHA-256 of the 64-byte `prev` field, the big-endian sequence number, and the raw event bytes. Sequence numbers MUST start at one and increase by one. The first `prev` value MUST be 64 zeroes. A line MUST NOT exceed 16 MiB.

A payload larger than 64 KiB MUST be stored under `blobs/<sha256>`. The event MUST contain the SHA-256 name and byte count. Blob writes MUST use a temporary file, file `fsync`, rename, and directory `fsync`.

## Append

`Append` MUST validate every event before writing. It MUST encode all events for one call and write them with one append write. It MUST `fsync` the journal before it returns. It MUST insert the same records and thread head in one immediate SQLite transaction. JSONL remains authoritative when the mirror write fails. The store MUST retain the new JSONL head and report the mirror error.

On Linux, when `DALGONA_SYNCFS` is set, the journal `fsync` MUST be followed by `Syncfs` for that directory. On other systems the variable MUST have no effect.

## Open and repair

`Open` MUST verify JSONL framing, JSON shape, sequence numbers, CRC values, and the hash chain. The quarantine `<offset>` MUST be the byte offset of the first bad line. The following rules MUST apply.

| Finding | Required action |
| --- | --- |
| The final line has no newline, cannot be parsed, or has a CRC or hash failure | Copy the partial final record to `quarantine/<session>-<offset>.bin`. When that file exists, the store MUST write `quarantine/<session>-<offset>-<seq>.bin` where `<seq>` is a per-process sequence number. The store MUST NOT overwrite a file in quarantine. Truncate at the last good line. `fsync` the file and directory. Log `journal: <session>: torn tail of <n> bytes quarantined`. Apply the verified mirror-suffix rule afterward. |
| A record has a sequence gap, a duplicate sequence, or a bad link at any position, or another bad record before the final line | Use the mirror only when every row from the damage point through the mirror head verifies against the chain. Rebuild JSONL from the good prefix and those rows through a temporary file, rename, and directory `fsync`. Keep every later JSONL record that verifies against the resulting chain. Rebuild the mirror from the kept JSONL. Valid records past the mirror head MUST NOT be discarded. |
| The mirror cannot supply that complete verified suffix | Mark the thread damaged at the first bad sequence. Keep only the valid prefix for replay. Reject appends. Preserve the bad suffix in quarantine before writing the damage marker. Report `journal: <session>: damaged at seq <n>; quarantined at quarantine/<file>`. |
| `state.db` fails `quick_check`, or a mirrored hash differs from JSONL | Move `state.db`, `state.db-wal`, and `state.db-shm` to `quarantine/state-<nanos>-<pid>-<seq>.db`, `quarantine/state-<nanos>-<pid>-<seq>.db-wal`, and `quarantine/state-<nanos>-<pid>-<seq>.db-shm`. `<nanos>` is Unix time in nanoseconds, `<pid>` is the process id, and `<seq>` is a per-process sequence number. The three files of one quarantine MUST share one stem. When a candidate name exists, the store MUST use the next free name with `-<n>` added to the stem. The store MUST NOT overwrite a file in quarantine. Rebuild the mirror from JSONL. |
| The mirror is behind JSONL | Replace the mirror rows of the thread with the JSONL rows. |
| The mirror is ahead of a complete JSONL file | Append the mirror suffix only when every row verifies from the JSONL head. Otherwise rebuild the mirror from JSONL. |

`Replay` MUST return every valid prefix event before it returns a damage error. A damaged thread MUST remain read-only across later opens. The store MUST NOT silently discard a committed event.

For a damaged thread, `Append` MUST fail with `journal: session <id> is damaged at seq <n>`. When the thread journal file is absent and the thread has committed events, `Replay` MUST fail with `journal: session <id> journal missing`.

## SQLite mirror

The database is `state.db`. The backup is `state.db.bak`. The store MUST enable WAL, `synchronous=FULL`, foreign keys, and a 5000 ms busy timeout. It MUST use this schema:

```sql
CREATE TABLE events(session TEXT NOT NULL, seq INTEGER NOT NULL, hash TEXT NOT NULL, ev BLOB NOT NULL, PRIMARY KEY(session, seq));
CREATE TABLE sessions(id TEXT PRIMARY KEY, parent TEXT, cwd TEXT, title TEXT, status TEXT NOT NULL CHECK(status IN ('ok','damaged')), head_seq INTEGER NOT NULL CHECK(head_seq >= 0), updated_at TEXT);
```

The database MUST record its schema version in `PRAGMA user_version`. `openDB` MUST apply ordered migrations at open and MUST reject a database whose version exceeds the supported version. Databases without a version adopt version 1 with no row changes. The version-2 migration MUST coerce legacy thread rows to the constrained shape. It MUST map a NULL or unexpected `status` to `damaged`. It MUST map a NULL or negative `head_seq` to 0. Loads MUST apply the same coercion to any row that still violates the constraint.

Every 30 minutes and at clean close, the store MUST run `VACUUM INTO` into `state.db.bak.tmp`. It MUST then rename that file to `state.db.bak`. The store MUST hold an exclusive lock on `repair.lock` from the removal of a stale `state.db.bak.tmp` through the rename. A clean close MUST report a backup failure. When `state.db` is unusable, the store MUST try `state.db.bak` before it creates an empty mirror. This rule allows a verified backup row to repair damaged JSONL.

## Locks

`Open` MUST hold an exclusive process lock on `daemon.lock`. A second owner MUST receive `another dalgona process owns <home>`. `Open` repairs every thread and can replace `state.db`. A home that `Open` uses MUST NOT have `OpenShared` users.

`LockSession` MUST hold an exclusive lock at `sessions/<id>/lock`. It MUST take that lock while it holds an exclusive lock on `sessions/<id>/repair.lock`, and it MUST wait for that gate. A second owner MUST receive `another dal process owns session <id>`. The caller MUST call the returned unlock function.

## Shared open

`OpenShared` lets many processes use one home. It MUST NOT lock `daemon.lock`. It relies on thread locks for journal writes. The caller MUST hold `LockSession` for a thread before it calls `Append` for that thread.

- Mirror join: `OpenShared` MUST hold an exclusive lock on `repair.lock` while it opens `state.db`. Under that lock it MUST try an exclusive lock on `users.lock` without a wait. When it gets the lock, no other shared store is open. Only then MAY it apply the `state.db` quarantine and backup rules. It MUST then release the exclusive lock and take a shared lock on `users.lock` before it releases `repair.lock`. It MUST hold the shared lock until close. When another shared store is open and `state.db` is unusable, `OpenShared` MUST fail and MUST NOT move `state.db`.
- Thread repair: the store MUST apply the repair table to a thread only while a thread lock is held. At open and in `Replay`, it MUST hold `sessions/<id>/repair.lock` and try `sessions/<id>/lock` without a wait. It MUST skip the repair when another owner holds the lock. When the calling Store holds the thread lock, the probe reports that lock as busy. That Store MUST skip the probe and reload the thread directly. Any other Store MUST observe the lock as busy and skip the repair. A damaged thread then stays read-only. `Append` MUST trust the caller's thread lock.
- Mirror repair: a mirror hash mismatch or an unverifiable mirror suffix MUST replace the rows of that thread from JSONL. The store MUST NOT quarantine or rebuild the whole `state.db` in shared mode.
- Freshness: before `Append`, the store MUST compare the journal file identity, size, and modification time with the values after its last load or write. On a difference, it MUST reload the thread through the repair table. Before the reload repairs the thread file, the store MUST close its own append handle for that thread. `Sessions` MUST reload the thread rows from `state.db`.
- Replay: `Replay` MUST stop without error at a final line that has no `\n`. Another owner can be in the middle of that append.

In both modes, the store MUST keep the mirror rows of a thread when they hold exactly the JSONL sequence numbers and hashes. It MUST then update only the thread row.

## Compaction

A compaction marker MUST keep each tool result with the assistant message that issued it. The scope of a call id or a run id is the part before "/". A cut MUST be safe only when no scope occurs on both sides of the cut. A cut MUST start at a user message or an assistant message.

The agent MUST NOT append a compaction whose `tokens_after` is not smaller than `tokens_before`. An automatic compaction that does not shrink MUST leave the turn running without a marker. After an automatic compaction that does not shrink, the agent MUST NOT run another automatic compaction until a compaction marker is appended. A manual or overflow compaction MUST fail with `compaction: the summary is not smaller than the context it replaces`.

## Blobs

Blob names MUST contain exactly 64 lowercase SHA-256 hexadecimal characters. `Blob` MUST reject malformed names and report missing blobs.
