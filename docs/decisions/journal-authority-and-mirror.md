# Journal authority and mirror

## Invariant

JSONL MUST be the event authority. SQLite MUST remain a repairable mirror. A successful JSONL append MUST NOT disappear because a mirror write fails. The store MUST validate the sequence, CRC, and hash chain before replay.

Repair MUST use verified records. A damaged session MUST retain its valid prefix and remain read-only. The store MUST quarantine damaged bytes. It MUST NOT discard a committed event without reporting the damage.

## Scope

This decision applies to `internal/journal` and all state derived from its events. [journal.md](../spec/journal.md) defines the record, append, repair, backup, and lock rules. [recovery.md](../spec/recovery.md) defines tool recovery. SQLite is not an independent source of session truth.

## Enforcement

`internal/journal/journal_test.go` checks truncated tails, middle-record repair, damage without a usable mirror, corrupt SQLite, restored tails, and writer exclusion. Event-fold tests in `internal/agent/event/state_test.go` check derived state. `just test` runs these tests with the race detector.
