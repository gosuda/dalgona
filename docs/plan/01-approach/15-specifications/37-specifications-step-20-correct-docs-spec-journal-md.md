#### Specifications step 20. Correct `docs/spec/journal.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/journal.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 34 | \| SQLite is behind JSONL \| Insert the missing rows from JSONL. \| | \| The mirror is behind JSONL \| Replace the mirror rows of the session with the JSONL rows. \| | internal/journal/repair.go:405-411 `if !s.shared && (plan.scan.rebuild \|\| mirrorHashMismatch(...))` then `return false, replaceSession(s.db, sid, plan.scan.emits, plan.head)`; sqlite.go:350-352 `replaceSession` runs `DELETE FROM events WHERE session = ?` |
| 65 | Freshness: before `Append`, the store MUST compare the journal file identity and size with the values after its last load or write. | Freshness: before `Append`, the store MUST compare the journal file identity, size, and modification time with the values after its last load or write. | internal/journal/shared.go:171-172 `same := os.SameFile(st.info, info) && st.info.Size() == info.Size() &&` / `st.info.ModTime().Equal(info.ModTime())` |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 37 | For a damaged session, `Append` MUST fail with `journal: session <id> is damaged at seq <n>`. | internal/journal/journal.go:317-318 `if st.damaged {` / `return st.seq, fmt.Errorf("journal: session %s is damaged at seq %d", session, st.damagedSeq)` |
| 37 | When the session journal file is absent and the session has committed events, `Replay` MUST fail with `journal: session <id> journal missing`. | internal/journal/journal.go:443-447 `if errors.Is(err, fs.ErrNotExist) {` / `if st == nil \|\| seq == 0 { return replaySnapshot{}, nil }` / `return replaySnapshot{}, fmt.Errorf("journal: session %s journal missing", sessio |

Use one word for one thing.

- Keep `mirror`. Replace Use the word SQLite only for the engine (line 20, `one immediate SQLite transaction`). Elsewhere: line 3 `SQLite is a repairable mirror.` becomes `The mirror is `state.db`. It is repairable from JSONL.`; line 30 `SQLite-suffix` becomes `mirror-suffix`; line 31 `Use SQLite only` becomes `Use the mirror only`, and `through the SQLite head` becomes `through the mirror head`; line 32 `SQLite cannot supply` becomes `The mirror cannot supply`; line 33 `SQLite `quick_check` fails or` becomes `` `state.db` fails `quick_check`, or ``; line 34 `SQLite is behind JSONL` becomes `The mirror is behind JSONL` (the replaced statement above); line 35 `SQLite is ahead of` becomes `The mirror is ahead of`, and `Append the SQLite suffix` becomes `Append the mirror suffix`; line 64 `unverifiable SQLite suffix` becomes `unverifiable mirror suffix`. at lines 3, 30, 31, 32, 33, 34, 35, 64.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 48 | The version-2 migration MUST coerce legacy session rows to the constrained shape. It MUST map a NULL or unexpected `status` to `damaged`. It MUST map a NULL or negative `head_seq` to 0. |
| 63 | When the calling Store holds the session lock, the probe reports that lock as busy. That Store MUST skip the probe and reload the session directly. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 2 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/journal.md` prints `style findings: 0`, and `just verify` passes.

