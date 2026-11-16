package journal

import (
	"testing"
)

func TestBackupSyncsDirectory(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	defer closeStore(t, store)
	synced := 0
	store.dirSync = func(string) error {
		synced++
		return nil
	}
	if err := store.backupDB(); err != nil {
		t.Fatalf("backupDB: %v", err)
	}
	if synced != 1 {
		t.Fatalf("backupDB synced the directory %d times, want 1", synced)
	}
}
