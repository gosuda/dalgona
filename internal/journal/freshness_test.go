package journal

import (
	"os"
	"testing"
)

func TestFreshnessDetectsSameSizeRewrite(t *testing.T) {
	dir := t.TempDir()
	store := mustOpenShared(t, dir)
	defer closeStore(t, store)
	release := mustLockSession(t, dir, "s")
	appendEvents(t, store, "s", 4)
	mustRelease(t, release)

	current, err := store.isCurrent("s")
	if err != nil {
		t.Fatalf("isCurrent: %v", err)
	}
	if !current {
		t.Fatal("fresh session reads as stale")
	}
	rewriteSameSize(t, store.journalPath("s"))
	current, err = store.isCurrent("s")
	if err != nil {
		t.Fatalf("isCurrent: %v", err)
	}
	if current {
		t.Fatal("same size rewrite with identical inode still reads as current")
	}
}

func rewriteSameSize(t *testing.T, path string) {
	t.Helper()
	original := readFile(t, path)
	replacement := make([]byte, len(original))
	copy(replacement, original)
	replacement[len(replacement)-2] ^= 0xff
	file, openErr := os.OpenFile(path, os.O_WRONLY|os.O_TRUNC, 0o600)
	if openErr != nil {
		t.Fatalf("open journal: %v", openErr)
	}
	if _, writeErr := file.Write(replacement); writeErr != nil {
		_ = file.Close()
		t.Fatalf("rewrite journal: %v", writeErr)
	}
	if syncErr := file.Sync(); syncErr != nil {
		_ = file.Close()
		t.Fatalf("sync journal: %v", syncErr)
	}
	if closeErr := file.Close(); closeErr != nil {
		t.Fatalf("close journal: %v", closeErr)
	}
}
