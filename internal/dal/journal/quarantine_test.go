package journal

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func quarantineState(t *testing.T, dir, content string) {
	t.Helper()
	path := filepath.Join(dir, "state.db")
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatalf("write state.db: %v", err)
	}
}

func quarantineBodies(t *testing.T, quarantine string) map[string]bool {
	t.Helper()
	matches, err := filepath.Glob(filepath.Join(quarantine, "state-*.db"))
	if err != nil {
		t.Fatalf("glob quarantine: %v", err)
	}
	bodies := map[string]bool{}
	for _, match := range matches {
		data, err := os.ReadFile(match)
		if err != nil {
			t.Fatalf("read %s: %v", match, err)
		}
		bodies[string(data)] = true
	}
	return bodies
}

func TestQuarantinePreservesRepeatedCopies(t *testing.T) {
	dir := t.TempDir()
	quarantine := filepath.Join(dir, "quarantine")
	if err := os.MkdirAll(quarantine, 0o700); err != nil {
		t.Fatalf("create quarantine: %v", err)
	}
	quarantineState(t, dir, "generation-one")
	if err := quarantineDB(dir, quarantine); err != nil {
		t.Fatalf("first quarantine: %v", err)
	}
	quarantineState(t, dir, "generation-two")
	if err := quarantineDB(dir, quarantine); err != nil {
		t.Fatalf("second quarantine: %v", err)
	}
	bodies := quarantineBodies(t, quarantine)
	if !bodies["generation-one"] || !bodies["generation-two"] {
		t.Fatalf("quarantine kept %v, want both generations preserved", bodies)
	}
}

func TestQuarantineKeepsGroupStem(t *testing.T) {
	dir := t.TempDir()
	quarantine := filepath.Join(dir, "quarantine")
	if err := os.MkdirAll(quarantine, 0o700); err != nil {
		t.Fatalf("create quarantine: %v", err)
	}
	quarantineState(t, dir, "grouped")
	wal := filepath.Join(dir, "state.db-wal")
	if err := os.WriteFile(wal, []byte("wal"), 0o600); err != nil {
		t.Fatalf("write state.db-wal: %v", err)
	}
	if err := quarantineDB(dir, quarantine); err != nil {
		t.Fatalf("quarantine: %v", err)
	}
	matches, err := filepath.Glob(filepath.Join(quarantine, "state-*.db*"))
	if err != nil {
		t.Fatalf("glob quarantine: %v", err)
	}
	if len(matches) != 2 {
		t.Fatalf("quarantine kept %d files, want the db and its wal", len(matches))
	}
	stem := ""
	for _, match := range matches {
		base, _ := strings.CutSuffix(filepath.Base(match), "-wal")
		base, _ = strings.CutSuffix(base, "-shm")
		if stem == "" {
			stem = base
		} else if stem != base {
			t.Fatalf("quarantine split the group across %v", matches)
		}
	}
}

func TestQuarantinePreservesSameTimestampCopies(t *testing.T) {
	prev := quarantineNow
	quarantineNow = func() time.Time { return time.Unix(1700000000, 0) }
	t.Cleanup(func() { quarantineNow = prev })
	dir := t.TempDir()
	quarantine := filepath.Join(dir, "quarantine")
	if err := os.MkdirAll(quarantine, 0o700); err != nil {
		t.Fatalf("create quarantine: %v", err)
	}
	quarantineState(t, dir, "generation-one")
	if err := quarantineDB(dir, quarantine); err != nil {
		t.Fatalf("first quarantine: %v", err)
	}
	quarantineState(t, dir, "generation-two")
	if err := quarantineDB(dir, quarantine); err != nil {
		t.Fatalf("second quarantine: %v", err)
	}
	bodies := quarantineBodies(t, quarantine)
	if !bodies["generation-one"] || !bodies["generation-two"] {
		t.Fatalf("quarantine kept %v, want both generations preserved", bodies)
	}
	matches, err := filepath.Glob(filepath.Join(quarantine, "state-*.db"))
	if err != nil {
		t.Fatalf("glob quarantine: %v", err)
	}
	if len(matches) != 2 {
		t.Fatalf("quarantine kept %d files, want both generations readable", len(matches))
	}
}

func quarantineBinBodies(t *testing.T, quarantine, pattern string) map[string]bool {
	t.Helper()
	matches, globErr := filepath.Glob(filepath.Join(quarantine, pattern))
	if globErr != nil {
		t.Fatalf("glob quarantine: %v", globErr)
	}
	bodies := map[string]bool{}
	for _, match := range matches {
		data, readErr := os.ReadFile(match)
		if readErr != nil {
			t.Fatalf("read %s: %v", match, readErr)
		}
		bodies[string(data)] = true
	}
	return bodies
}

func TestQuarantineSuffixPreservesRepeatedOffsets(t *testing.T) {
	dir := t.TempDir()
	quarantine := filepath.Join(dir, "quarantine")
	if err := os.MkdirAll(quarantine, 0o700); err != nil {
		t.Fatalf("create quarantine: %v", err)
	}
	store := &Store{dir: dir}
	first := &scanState{orig: []byte("01234567-one"), stopOff: 8, torn: true}
	if err := store.quarantineSuffix("s", first); err != nil {
		t.Fatalf("first suffix quarantine: %v", err)
	}
	second := &scanState{orig: []byte("01234567-two"), stopOff: 8, torn: true}
	if err := store.quarantineSuffix("s", second); err != nil {
		t.Fatalf("second suffix quarantine: %v", err)
	}
	seen := quarantineBinBodies(t, quarantine, "s-*.bin")
	if !seen["-one"] || !seen["-two"] {
		t.Fatalf("quarantine kept %v, want both suffix generations preserved", seen)
	}
	exact, err := os.ReadFile(filepath.Join(quarantine, "s-8.bin"))
	if err != nil {
		t.Fatalf("read s-8.bin: %v", err)
	}
	if string(exact) != "-one" {
		t.Fatalf("quarantine/s-8.bin holds %q, want %q", exact, "-one")
	}
}
