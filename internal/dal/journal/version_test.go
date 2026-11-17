package journal

import (
	"path/filepath"
	"testing"

	"github.com/cockroachdb/pebble/v2"
)

func queryMirrorVersion(t *testing.T, m *mirrorDB) string {
	t.Helper()
	var version string
	if err := m.withDB(func(db *pebble.DB) error {
		value, closer, err := db.Get(keyVersion)
		if err != nil {
			return err
		}
		defer func() { _ = closer.Close() }()
		version = string(value)
		return nil
	}); err != nil {
		t.Fatalf("mirror version: %v", err)
	}
	return version
}

func openRawMirror(t *testing.T, dir string) *pebble.DB {
	t.Helper()
	raw, err := pebble.Open(filepath.Join(dir, "state.db"), nil)
	if err != nil {
		t.Fatalf("open raw state.db: %v", err)
	}
	return raw
}

func setMirrorVersion(t *testing.T, dir, version string) {
	t.Helper()
	raw := openRawMirror(t, dir)
	if version == "" {
		if err := raw.Delete(keyVersion, pebble.Sync); err != nil {
			_ = raw.Close()
			t.Fatalf("delete mirror version: %v", err)
		}
	} else if err := raw.Set(keyVersion, []byte(version), pebble.Sync); err != nil {
		_ = raw.Close()
		t.Fatalf("set mirror version: %v", err)
	}
	if err := raw.Close(); err != nil {
		t.Fatalf("close raw state.db: %v", err)
	}
}

func TestMirrorSchemaCarriesVersion(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	version := queryMirrorVersion(t, store.mirror)
	closeStore(t, store)
	if version != mirrorSchemaVersion {
		t.Fatalf("mirror version = %q, want %s", version, mirrorSchemaVersion)
	}
}

func TestMirrorMigratesLegacyVersion(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	appendEvents(t, store, "s", 2)
	closeStore(t, store)
	setMirrorVersion(t, dir, "")
	reopened := mustOpen(t, dir)
	defer closeStore(t, reopened)
	if version := queryMirrorVersion(t, reopened.mirror); version != mirrorSchemaVersion {
		t.Fatalf("mirror version = %q, want %s after adoption", version, mirrorSchemaVersion)
	}
	head, err := reopened.Status("s")
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	if head.HeadSeq != 2 || head.Status != SessionOK {
		t.Fatalf("Status = %+v, want head 2 ok", head)
	}
}

func TestMirrorRejectsFutureVersion(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	events := appendEvents(t, store, "s", 2)
	closeStore(t, store)
	setMirrorVersion(t, dir, "9")
	reopened := mustOpen(t, dir)
	defer closeStore(t, reopened)
	if version := queryMirrorVersion(t, reopened.mirror); version != mirrorSchemaVersion {
		t.Fatalf("mirror version = %q, want %s after rejecting future version", version, mirrorSchemaVersion)
	}
	matches, err := filepath.Glob(filepath.Join(dir, "quarantine", "state-*.db"))
	if err != nil || len(matches) == 0 {
		t.Fatalf("future-version state.db was not quarantined: %v, %v", matches, err)
	}
	head, err := reopened.Status("s")
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	if head.HeadSeq != 2 || head.Status != SessionOK {
		t.Fatalf("Status = %+v, want head 2 ok", head)
	}
	requireEvents(t, replayEvents(t, reopened, "s"), events)
}
