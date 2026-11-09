package journal

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
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
