//go:build windows

package journal

import (
	"errors"
	"os"

	"golang.org/x/sys/windows"
)

func lockRange(f *os.File, flags uint32) error {
	overlapped := &windows.Overlapped{}
	return windows.LockFileEx(windows.Handle(f.Fd()), flags, 0, 1, 0, overlapped)
}

func lockFile(f *os.File) error {
	return lockRange(f, windows.LOCKFILE_EXCLUSIVE_LOCK|windows.LOCKFILE_FAIL_IMMEDIATELY)
}

func waitFile(f *os.File) error {
	return lockRange(f, windows.LOCKFILE_EXCLUSIVE_LOCK)
}

func waitFileShared(f *os.File) error {
	return lockRange(f, 0)
}

func unlockFile(f *os.File) error {
	overlapped := &windows.Overlapped{}
	return windows.UnlockFileEx(windows.Handle(f.Fd()), 0, 1, 0, overlapped)
}

func isLockBusy(err error) bool {
	return errors.Is(err, windows.ERROR_LOCK_VIOLATION) ||
		errors.Is(err, windows.ERROR_IO_PENDING)
}
