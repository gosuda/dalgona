package journal

import (
	"bytes"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"log/slog"
	"math/rand/v2"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

func event(n int) []byte {
	return []byte(`{"type":"test","n":` + strconv.Itoa(n) + `}`)
}

func mustOpen(t *testing.T, dir string) *Store {
	t.Helper()
	store, err := Open(dir)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	return store
}

func closeStore(t *testing.T, store *Store) {
	t.Helper()
	if err := store.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
}

func appendEvents(t *testing.T, store *Store, id string, count int) [][]byte {
	t.Helper()
	events := make([][]byte, count)
	for i := range events {
		events[i] = event(i)
	}
	if _, err := store.Append(context.Background(), id, events...); err != nil {
		t.Fatalf("Append: %v", err)
	}
	return events
}

func replayEvents(t *testing.T, store *Store, id string) [][]byte {
	t.Helper()
	var out [][]byte
	err := store.Replay(id, func(_ uint64, ev []byte) error {
		out = append(out, bytes.Clone(ev))
		return nil
	})
	if err != nil {
		t.Fatalf("Replay: %v", err)
	}
	return out
}

func requireEvents(t *testing.T, got, want [][]byte) {
	t.Helper()
	if len(got) != len(want) {
		t.Fatalf("event count = %d, want %d", len(got), len(want))
	}
	for i := range want {
		if !bytes.Equal(got[i], want[i]) {
			t.Fatalf("event %d = %s, want %s", i, got[i], want[i])
		}
	}
}

func readFile(t *testing.T, path string) []byte {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}
	return data
}

func writeFile(t *testing.T, path string, data []byte) {
	t.Helper()
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}
}

func journalFile(dir string) string {
	return filepath.Join(dir, "sessions", "s", "journal.jsonl")
}

func TestJournalTruncatedTail(t *testing.T) {
	rng := rand.New(rand.NewPCG(42, 42))
	for caseID := range 500 {
		t.Run(fmt.Sprintf("case-%d", caseID), func(t *testing.T) {
			dir := t.TempDir()
			store := mustOpen(t, dir)
			events := appendEvents(t, store, "s", 1+rng.IntN(12))
			closeStore(t, store)
			path := journalFile(dir)
			original := readFile(t, path)
			starts := lineStarts(original)
			line := rng.IntN(len(events))
			start := starts[line]
			end := start + bytes.IndexByte(original[start:], '\n')
			offset := start + 1 + rng.IntN(end-start-1)
			writeFile(t, path, original[:offset])
			deleteMirrorFrom(t, dir, line+1)

			store = mustOpen(t, dir)
			got := replayEvents(t, store, "s")
			requireEvents(t, got, events[:line])
			seq, err := store.Append(context.Background(), "s", event(99))
			if err != nil {
				t.Fatalf("Append after repair: %v", err)
			}
			if got, want := strconv.FormatUint(seq, 10), strconv.Itoa(line+1); got != want {
				t.Fatalf("next seq = %s, want %s", got, want)
			}
			quarantine := filepath.Join(dir, "quarantine", fmt.Sprintf("s-%d.bin", start))
			if !bytes.Equal(readFile(t, quarantine), original[start:offset]) {
				t.Fatal("quarantined tail differs from removed bytes")
			}
			closeStore(t, store)
		})
	}
	t.Run("committed-suffix", testCommittedSuffix)
}

func testCommittedSuffix(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	events := appendEvents(t, store, "s", 5)
	closeStore(t, store)
	path := journalFile(dir)
	original := readFile(t, path)
	start := lineStarts(original)[1]
	writeFile(t, path, original[:start+3])

	store = mustOpen(t, dir)
	requireEvents(t, replayEvents(t, store, "s"), events)
	if restored := readFile(t, path); !bytes.Equal(restored, original) {
		t.Fatal("restored torn journal differs from the original")
	}
	seq, err := store.Append(context.Background(), "s", event(99))
	if err != nil {
		t.Fatal(err)
	}
	if got, want := strconv.FormatUint(seq, 10), strconv.Itoa(len(events)+1); got != want {
		t.Fatalf("next seq = %s, want %s", got, want)
	}
	closeStore(t, store)
}

func lineStarts(data []byte) []int {
	starts := []int{0}
	for i, value := range data {
		if value == '\n' && i+1 < len(data) {
			starts = append(starts, i+1)
		}
	}
	return starts
}

func corruptSecondLine(t *testing.T, path string) int {
	t.Helper()
	data := readFile(t, path)
	firstEnd := bytes.IndexByte(data, '\n')
	start := firstEnd + 1
	end := start + bytes.IndexByte(data[start:], '\n')
	rel := bytes.IndexByte(data[start:end], 't')
	if rel < 0 {
		t.Fatal("second record has no byte to corrupt")
	}
	data[start+rel] = 'x'
	writeFile(t, path, data)
	return start
}

func TestJournalMidlineRepair(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	events := appendEvents(t, store, "s", 5)
	closeStore(t, store)
	path := journalFile(dir)
	original := readFile(t, path)
	corruptSecondLine(t, path)

	store = mustOpen(t, dir)
	requireEvents(t, replayEvents(t, store, "s"), events)
	if repaired := readFile(t, path); !bytes.Equal(repaired, original) {
		t.Fatal("rebuilt journal is not byte-identical to the original")
	}
	closeStore(t, store)
}

func deleteMirrorFrom(t *testing.T, dir string, firstMissing int) {
	t.Helper()
	db, err := sql.Open("sqlite", dbDSN(filepath.Join(dir, "state.db")))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec("DELETE FROM events WHERE session = 's' AND seq >= ?", firstMissing); err != nil {
		t.Fatal(err)
	}
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
}

func requireDamageHead(t *testing.T, store *Store, headSeq uint64) {
	t.Helper()
	head, err := store.Status("s")
	if err != nil {
		t.Fatal(err)
	}
	if head.Status != SessionDamaged || head.DamagedSeq != 2 || head.HeadSeq != headSeq {
		t.Fatalf("head = %+v, want damage at seq 2 after %d", head, headSeq)
	}
}

func requireDamageReplay(t *testing.T, store *Store) {
	t.Helper()
	var prefix [][]byte
	replayErr := store.Replay("s", func(_ uint64, ev []byte) error {
		prefix = append(prefix, bytes.Clone(ev))
		return nil
	})
	damage, ok := errors.AsType[*DamageError](replayErr)
	if !ok || damage.Seq != 2 {
		t.Fatalf("Replay error = %v, want damage at seq 2", replayErr)
	}
	requireEvents(t, prefix, [][]byte{event(0)})
}

func TestJournalDamageWithoutMirror(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	appendEvents(t, store, "s", 5)
	closeStore(t, store)
	offset := corruptSecondLine(t, journalFile(dir))
	deleteMirrorFrom(t, dir, 2)
	var logs bytes.Buffer
	previousLogger := slog.Default()
	slog.SetDefault(slog.New(slog.NewTextHandler(&logs, nil)))
	t.Cleanup(func() { slog.SetDefault(previousLogger) })

	store = mustOpen(t, dir)
	requireDamageHead(t, store, 1)
	requireDamageReplay(t, store)
	if _, err := store.Append(context.Background(), "s", event(9)); err == nil {
		t.Fatal("damaged session accepted an append")
	}
	quarantineName := fmt.Sprintf("s-%d.bin", offset)
	wantLog := "journal: s: damaged at seq 2; quarantined at quarantine/" + quarantineName
	if !strings.Contains(logs.String(), wantLog) {
		t.Fatalf("log %q does not contain %q", logs.String(), wantLog)
	}
	if _, err := os.Stat(filepath.Join(dir, "quarantine", quarantineName)); err != nil {
		t.Fatal(err)
	}
	closeStore(t, store)
	store = mustOpen(t, dir)
	requireDamageHead(t, store, 1)
	closeStore(t, store)
}

func TestJournalGarbageDB(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	events := appendEvents(t, store, "s", 2)
	closeStore(t, store)
	writeFile(t, filepath.Join(dir, "state.db"), []byte("garbage"))

	store = mustOpen(t, dir)
	requireEvents(t, replayEvents(t, store, "s"), events)
	entries, err := os.ReadDir(filepath.Join(dir, "quarantine"))
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, entry := range entries {
		found = found || strings.HasPrefix(entry.Name(), "state-") && strings.HasSuffix(entry.Name(), ".db")
	}
	if !found {
		t.Fatal("corrupt database was not quarantined")
	}
	closeStore(t, store)
	t.Run("backup-reconciles-double-failure", testBackupReconciliation)
}

func testBackupReconciliation(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	events := appendEvents(t, store, "s", 5)
	closeStore(t, store)
	path := journalFile(dir)
	original := readFile(t, path)
	corruptSecondLine(t, path)
	writeFile(t, filepath.Join(dir, "state.db"), []byte("garbage"))

	store = mustOpen(t, dir)
	requireEvents(t, replayEvents(t, store, "s"), events)
	if repaired := readFile(t, path); !bytes.Equal(repaired, original) {
		t.Fatal("backup-reconciled journal differs from the original")
	}
	closeStore(t, store)
}

func TestJournalRestoreTail(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	events := appendEvents(t, store, "s", 5)
	closeStore(t, store)
	path := journalFile(dir)
	original := readFile(t, path)
	truncated := bytes.Clone(original)
	for range 3 {
		last := bytes.LastIndexByte(truncated[:len(truncated)-1], '\n')
		truncated = truncated[:last+1]
	}
	writeFile(t, path, truncated)

	store = mustOpen(t, dir)
	requireEvents(t, replayEvents(t, store, "s"), events)
	if restored := readFile(t, path); !bytes.Equal(restored, original) {
		t.Fatal("restored journal differs from the original")
	}
	closeStore(t, store)
}

func TestJournalSecondOpen(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	second, err := Open(dir)
	if second != nil {
		closeStore(t, second)
	}
	if err == nil || err.Error() != "another dalgona process owns "+dir {
		t.Fatalf("second Open error = %v", err)
	}
	closeStore(t, store)
	if _, err := os.Stat(filepath.Join(dir, "state.db.bak")); err != nil {
		t.Fatalf("clean close did not create a backup: %v", err)
	}
	store = mustOpen(t, dir)
	closeStore(t, store)
}

func TestJournalSyncfs(t *testing.T) {
	dir := t.TempDir()
	previousHook := syncfsHook
	t.Cleanup(func() { syncfsHook = previousHook })
	var paths []string
	syncfsHook = func(path string) error {
		paths = append(paths, path)
		return nil
	}
	t.Setenv("DALGONA_SYNCFS", dir)
	store := mustOpen(t, dir)
	seq, err := store.Append(context.Background(), "s", event(1))
	if err != nil || seq != 1 {
		t.Fatalf("Append seq = %d, err = %v", seq, err)
	}
	if len(paths) != 1 || paths[0] != dir {
		t.Fatalf("Syncfs paths = %v, want [%s]", paths, dir)
	}
	closeStore(t, store)
}
