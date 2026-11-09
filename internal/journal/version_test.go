package journal

import (
	"database/sql"
	"path/filepath"
	"testing"
)

func queryUserVersion(t *testing.T, db *sql.DB) int {
	t.Helper()
	var version int
	if err := db.QueryRow("PRAGMA user_version").Scan(&version); err != nil {
		t.Fatalf("PRAGMA user_version: %v", err)
	}
	return version
}

func openRaw(t *testing.T, dir string) *sql.DB {
	t.Helper()
	raw, err := sql.Open("sqlite", dbDSN(filepath.Join(dir, "state.db")))
	if err != nil {
		t.Fatalf("open raw state.db: %v", err)
	}
	return raw
}

func execAll(t *testing.T, raw *sql.DB, stmts []string) {
	t.Helper()
	for _, stmt := range stmts {
		if _, err := raw.Exec(stmt); err != nil {
			_ = raw.Close()
			t.Fatalf("legacy setup %q: %v", stmt, err)
		}
	}
}

func TestMirrorSchemaCarriesVersion(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	version := queryUserVersion(t, store.db)
	closeStore(t, store)
	if version != 1 {
		t.Fatalf("user_version = %d, want 1", version)
	}
}

func TestMirrorMigratesLegacyVersion(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	appendEvents(t, store, "s", 2)
	closeStore(t, store)
	raw := openRaw(t, dir)
	execAll(t, raw, []string{"PRAGMA user_version = 0"})
	if err := raw.Close(); err != nil {
		t.Fatalf("close raw state.db: %v", err)
	}
	reopened := mustOpen(t, dir)
	defer closeStore(t, reopened)
	if version := queryUserVersion(t, reopened.db); version != 1 {
		t.Fatalf("user_version = %d, want 1 after migration", version)
	}
	head, err := reopened.Status("s")
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	if head.HeadSeq != 2 || head.Status != SessionOK {
		t.Fatalf("Status = %+v, want head 2 ok", head)
	}
}
