package journal

import (
	"database/sql"
	"strings"
	"testing"
)

const legacySessionsDDL = `CREATE TABLE sessions(id TEXT PRIMARY KEY, parent TEXT, cwd TEXT, title TEXT, status TEXT, head_seq INTEGER, updated_at TEXT)`

func querySessionsDDL(t *testing.T, db *sql.DB) string {
	t.Helper()
	var ddl sql.NullString
	if err := db.QueryRow("SELECT sql FROM sqlite_master WHERE name = 'sessions'").Scan(&ddl); err != nil {
		t.Fatalf("sessions DDL: %v", err)
	}
	return ddl.String
}

func assertLegacyRowsDamaged(t *testing.T, store *Store) {
	t.Helper()
	heads, err := store.Sessions()
	if err != nil {
		t.Fatalf("Sessions: %v", err)
	}
	byID := map[string]SessionHead{}
	for _, head := range heads {
		byID[head.ID] = head
	}
	for _, id := range []string{"null-row", "bogus-row"} {
		head, ok := byID[id]
		if !ok {
			t.Fatalf("session %s missing after migration", id)
		}
		if head.Status != SessionDamaged {
			t.Fatalf("session %s status = %s, want damaged", id, head.Status)
		}
		if head.HeadSeq != 0 {
			t.Fatalf("session %s head = %d, want 0", id, head.HeadSeq)
		}
	}
}

func TestMirrorMigratesLegacyRows(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	closeStore(t, store)
	raw := openRaw(t, dir)
	execAll(t, raw, []string{
		"DROP TABLE sessions",
		legacySessionsDDL,
		"INSERT INTO sessions(id, status, head_seq) VALUES('null-row', NULL, NULL)",
		"INSERT INTO sessions(id, status, head_seq) VALUES('bogus-row', 'bogus', -5)",
		"PRAGMA user_version = 0",
	})
	if err := raw.Close(); err != nil {
		t.Fatalf("close raw state.db: %v", err)
	}
	reopened := mustOpen(t, dir)
	defer closeStore(t, reopened)
	if version := queryUserVersion(t, reopened.db); version != 2 {
		t.Fatalf("user_version = %d, want 2 after migration", version)
	}
	assertLegacyRowsDamaged(t, reopened)
	ddl := querySessionsDDL(t, reopened.db)
	if !strings.Contains(ddl, "NOT NULL") || !strings.Contains(ddl, "CHECK") {
		t.Fatalf("sessions DDL lacks constraints: %s", ddl)
	}
}

func TestMirrorMigratesV1LegacyShapeToV2(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	closeStore(t, store)
	raw := openRaw(t, dir)
	execAll(t, raw, []string{
		"DROP TABLE sessions",
		legacySessionsDDL,
		"INSERT INTO sessions(id, status, head_seq) VALUES('null-row', NULL, NULL)",
		"INSERT INTO sessions(id, status, head_seq) VALUES('bogus-row', 'bogus', -5)",
		"PRAGMA user_version = 1",
	})
	if err := raw.Close(); err != nil {
		t.Fatalf("close raw state.db: %v", err)
	}
	reopened := mustOpen(t, dir)
	if version := queryUserVersion(t, reopened.db); version != 2 {
		closeStore(t, reopened)
		t.Fatalf("user_version = %d, want 2 after migration", version)
	}
	assertLegacyRowsDamaged(t, reopened)
	ddl := querySessionsDDL(t, reopened.db)
	if !strings.Contains(ddl, "NOT NULL") || !strings.Contains(ddl, "CHECK") {
		closeStore(t, reopened)
		t.Fatalf("sessions DDL lacks constraints: %s", ddl)
	}
	closeStore(t, reopened)
	again := mustOpen(t, dir)
	defer closeStore(t, again)
	if version := queryUserVersion(t, again.db); version != 2 {
		t.Fatalf("user_version = %d, want 2 on second open", version)
	}
	assertLegacyRowsDamaged(t, again)
}
