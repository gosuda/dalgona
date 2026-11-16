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
	t.Cleanup(func() {
		_ = store.Close()
	})
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
	t.Parallel()
	rng := rand.New(rand.NewPCG(42, 42))
	fixtures := make(map[int]string, 12)
	for count := 1; count <= 12; count++ {
		fixtures[count] = buildTruncFixture(t, count)
	}
	specs := make([]truncSpec, 500)
	for i := range specs {
		specs[i] = planTruncCase(t, rng, fixtures)
	}
	sem := make(chan struct{}, 8)
	for caseID, spec := range specs {
		t.Run(fmt.Sprintf("case-%d", caseID), func(t *testing.T) {
			t.Parallel()
			sem <- struct{}{}
			defer func() { <-sem }()
			runTruncCase(t, fixtures[spec.count], spec)
		})
	}
	t.Run("committed-suffix", testCommittedSuffix)
}

type truncSpec struct {
	count  int
	line   int
	offset int
}

func buildTruncFixture(t *testing.T, count int) string {
	t.Helper()
	dir := t.TempDir()
	store := mustOpen(t, dir)
	appendEvents(t, store, "s", count)
	closeStore(t, store)
	return dir
}

func planTruncCase(t *testing.T, rng *rand.Rand, fixtures map[int]string) truncSpec {
	t.Helper()
	count := 1 + rng.IntN(12)
	original := readFile(t, journalFile(fixtures[count]))
	starts := lineStarts(original)
	line := rng.IntN(count)
	start := starts[line]
	end := start + bytes.IndexByte(original[start:], '\n')
	offset := start + 1 + rng.IntN(end-start-1)
	return truncSpec{count: count, line: line, offset: offset}
}

func runTruncCase(t *testing.T, fixture string, spec truncSpec) {
	t.Helper()
	dir := t.TempDir()
	copyDir(t, fixture, dir)
	path := journalFile(dir)
	original := readFile(t, path)
	start := lineStarts(original)[spec.line]
	t.Logf("count=%d line=%d offset=%d start=%d", spec.count, spec.line, spec.offset, start)
	writeFile(t, path, original[:spec.offset])
	deleteMirrorFrom(t, dir, spec.line+1)
	store := mustOpen(t, dir)
	events := make([][]byte, spec.count)
	for i := range events {
		events[i] = event(i)
	}
	got := replayEvents(t, store, "s")
	requireEvents(t, got, events[:spec.line])
	seq, err := store.Append(context.Background(), "s", event(99))
	if err != nil {
		t.Fatalf("Append after repair: %v", err)
	}
	if got, want := strconv.FormatUint(seq, 10), strconv.Itoa(spec.line+1); got != want {
		t.Fatalf("next seq = %s, want %s", got, want)
	}
	quarantine := filepath.Join(dir, "quarantine", fmt.Sprintf("s-%d.bin", start))
	if !bytes.Equal(readFile(t, quarantine), original[start:spec.offset]) {
		t.Fatal("quarantined tail differs from removed bytes")
	}
	closeStore(t, store)
}

func copyDir(t *testing.T, src, dst string) {
	t.Helper()
	entries, err := os.ReadDir(src)
	if err != nil {
		t.Fatalf("ReadDir: %v", err)
	}
	for _, entry := range entries {
		s := filepath.Join(src, entry.Name())
		d := filepath.Join(dst, entry.Name())
		if entry.IsDir() {
			if err := os.MkdirAll(d, 0o700); err != nil {
				t.Fatalf("MkdirAll: %v", err)
			}
			copyDir(t, s, d)
			continue
		}
		if !entry.Type().IsRegular() {
			t.Fatalf("copy %s: not a regular file", s)
		}
		data, err := os.ReadFile(s)
		if err != nil {
			t.Fatalf("ReadFile: %v", err)
		}
		if err := os.WriteFile(d, data, 0o600); err != nil {
			t.Fatalf("WriteFile: %v", err)
		}
	}
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

func TestJournalKeepsValidTailPastMirrorHead(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	events := appendEvents(t, store, "s", 5)
	closeStore(t, store)
	path := journalFile(dir)
	original := readFile(t, path)
	corruptSecondLine(t, path)
	deleteMirrorFrom(t, dir, 4)

	store = mustOpen(t, dir)
	requireEvents(t, replayEvents(t, store, "s"), events)
	if repaired := readFile(t, path); !bytes.Equal(repaired, original) {
		t.Fatal("rebuilt journal is not byte-identical to the original")
	}
	seq, err := store.Append(context.Background(), "s", event(99))
	if err != nil {
		t.Fatal(err)
	}
	if seq != 6 {
		t.Fatalf("next seq = %d, want 6", seq)
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

func TestAppendFileRetriesDirSync(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	calls := 0
	store.dirSync = func(string) error {
		calls++
		if calls == 1 {
			return errors.New("injected dir sync failure")
		}
		return nil
	}
	if _, err := store.Append(t.Context(), "s", event(0)); err == nil {
		t.Fatal("Append with failing dir sync succeeded")
	}
	if _, err := store.Append(t.Context(), "s", event(1)); err != nil {
		t.Fatalf("Append after sync recovery: %v", err)
	}
	if calls != 3 {
		t.Fatalf("dir sync calls = %d, want 3 (fail once, retry both dirs)", calls)
	}
	requireEvents(t, replayEvents(t, store, "s"), [][]byte{event(1)})
	closeStore(t, store)
}
