//go:build windows

package journal

import (
	"errors"
	"fmt"
	"os"
	"time"

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

func waitFileBounded(f *os.File, d time.Duration) error {
	event, err := windows.CreateEvent(nil, 1, 0, nil)
	if err != nil {
		return err
	}
	defer func() { _ = windows.CloseHandle(event) }()
	overlapped := &windows.Overlapped{HEvent: event}
	if err := windows.LockFileEx(windows.Handle(f.Fd()), windows.LOCKFILE_EXCLUSIVE_LOCK, 0, 1, 0, overlapped); err != nil && !errors.Is(err, windows.ERROR_IO_PENDING) {
		return err
	}
	wait, err := windows.WaitForSingleObject(event, uint32(d.Milliseconds())) //nolint:gosec // 5000 ms fits uint32
	switch {
	case err != nil:
		return err
	case wait == windows.WAIT_OBJECT_0:
		var done uint32
		return windows.GetOverlappedResult(windows.Handle(f.Fd()), overlapped, &done, false)
	case wait == uint32(windows.WAIT_TIMEOUT):
		_ = windows.CancelIoEx(windows.Handle(f.Fd()), overlapped)
		var done uint32
		_ = windows.GetOverlappedResult(windows.Handle(f.Fd()), overlapped, &done, true)
		return errMirrorBusy
	default:
		return fmt.Errorf("journal: wait on state.db.lock: status %d", wait)
	}
}
