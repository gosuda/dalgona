//go:build windows

package journal

import (
	"errors"
	"os"
	"path/filepath"
	"syscall"
	"testing"
	"time"

	"golang.org/x/sys/windows"
)

func heldBackupFixture(t *testing.T) backupFixture {
	t.Helper()
	fixture := backupFixture{dir: t.TempDir()}
	fixture.final = filepath.Join(fixture.dir, "state.db.bak")
	fixture.staged = filepath.Join(fixture.dir, "state.db.bak.tmp.staged.0.1")
	fixture.tmp = filepath.Join(fixture.dir, "state.db.bak.tmp")
	for _, path := range []string{fixture.final, fixture.staged} {
		if err := os.MkdirAll(path, 0o700); err != nil {
			t.Fatal(err)
		}
	}
	writeFile(t, filepath.Join(fixture.final, "MARKER"), []byte("backup"))
	writeFile(t, filepath.Join(fixture.staged, "STAGED"), []byte("staged"))
	aged := time.Now().Add(-time.Hour)
	if err := os.Chtimes(fixture.final, aged, aged); err != nil {
		t.Fatal(err)
	}
	return fixture
}

func holdBackupDir(t *testing.T, path string) windows.Handle {
	t.Helper()
	name, err := windows.UTF16PtrFromString(path)
	if err != nil {
		t.Fatal(err)
	}
	handle, err := windows.CreateFile(name, windows.GENERIC_READ,
		windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE, nil,
		windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS, 0)
	if err != nil {
		t.Fatal(err)
	}
	return handle
}

func releaseBackupDirAfter(handle windows.Handle, delay time.Duration) <-chan struct{} {
	done := make(chan struct{})
	go func() {
		defer close(done)
		time.Sleep(delay)
		_ = windows.CloseHandle(handle)
	}()
	return done
}

func TestInstallBackupRetriesHeldStagedDir(t *testing.T) {
	fixture := heldBackupFixture(t)
	done := releaseBackupDirAfter(holdBackupDir(t, fixture.staged), 100*time.Millisecond)
	err := installBackup(fixture.dir, fixture.staged, fixture.tmp, syncDir)
	<-done
	if err != nil {
		t.Fatalf("install held staged checkpoint: %v", err)
	}
	if _, serr := os.Stat(filepath.Join(fixture.final, "STAGED")); serr != nil {
		t.Fatalf("held staged checkpoint was not installed: %v", serr)
	}
}

func TestInstallBackupHeldPastDeadlineKeepsBackup(t *testing.T) {
	fixture := heldBackupFixture(t)
	handle := holdBackupDir(t, fixture.staged)
	defer func() { _ = windows.CloseHandle(handle) }()
	err := installBackup(fixture.dir, fixture.staged, fixture.tmp, syncDir)
	if err == nil {
		t.Fatal("install held staged checkpoint succeeded, want sharing violation or access denied")
	}
	if !errors.Is(err, windows.ERROR_SHARING_VIOLATION) && !errors.Is(err, syscall.ERROR_ACCESS_DENIED) {
		t.Fatalf("install held staged checkpoint: %v, want sharing violation or access denied", err)
	}
	if _, serr := os.Stat(filepath.Join(fixture.final, "MARKER")); serr != nil {
		t.Fatalf("failed install lost the previous backup: %v", serr)
	}
}

func useBackupRename(t *testing.T, rename func(oldpath, newpath string) error) {
	t.Helper()
	backupRename = rename
	t.Cleanup(func() { backupRename = os.Rename })
}

func TestRenameBackupDirStartsNoRetryPastDeadline(t *testing.T) {
	held := &os.LinkError{Op: "rename", Old: "staged", New: "tmp", Err: windows.ERROR_SHARING_VIOLATION}
	attempts := 0
	var firstDone time.Time
	useBackupRename(t, func(string, string) error {
		attempts++
		switch attempts {
		case 1:
			firstDone = time.Now()
		case 2:
			time.Sleep(time.Until(firstDone.Add(backupRenameRetryDeadline - backupRenameRetryInterval/2)))
		}
		return held
	})
	if err := renameBackupDir("staged", "tmp"); !errors.Is(err, windows.ERROR_SHARING_VIOLATION) {
		t.Fatalf("rename held past the deadline: %v, want sharing violation", err)
	}
	if attempts != 2 {
		t.Fatalf("rename attempts = %d, want 2: no attempt may start after the deadline", attempts)
	}
}
