package journal

import (
	"testing"
)

func setRawSessionRow(t *testing.T, dir, id, value string) {
	t.Helper()
	raw := openRawMirror(t, dir)
	if err := raw.Set(keySession(id), []byte(value), nil); err != nil {
		_ = raw.Close()
		t.Fatalf("write session row: %v", err)
	}
	if err := raw.Close(); err != nil {
		t.Fatalf("close raw state.db: %v", err)
	}
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

func TestMirrorCoercesLegacyRows(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	closeStore(t, store)
	setRawSessionRow(t, dir, "null-row", "\x1f")
	setRawSessionRow(t, dir, "bogus-row", "bogus\x1f-5")
	reopened := mustOpen(t, dir)
	defer closeStore(t, reopened)
	assertLegacyRowsDamaged(t, reopened)
}
