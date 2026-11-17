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

func lockGateBounded(path string, d time.Duration) (*os.File, error) {
	name, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return nil, err
	}
	handle, err := windows.CreateFile(name, windows.GENERIC_READ|windows.GENERIC_WRITE,
		windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE|windows.FILE_SHARE_DELETE,
		nil, windows.OPEN_ALWAYS, windows.FILE_ATTRIBUTE_NORMAL|windows.FILE_FLAG_OVERLAPPED, 0)
	if err != nil {
		return nil, fmt.Errorf("journal: open %s: %w", path, err)
	}
	file := os.NewFile(uintptr(handle), path)
	if file == nil {
		_ = windows.CloseHandle(handle)
		return nil, fmt.Errorf("journal: open %s: invalid handle", path)
	}
	event, err := windows.CreateEvent(nil, 1, 0, nil)
	if err != nil {
		return nil, errors.Join(err, file.Close())
	}
	defer func() { _ = windows.CloseHandle(event) }()
	overlapped := &windows.Overlapped{HEvent: event}
	err = windows.LockFileEx(handle, windows.LOCKFILE_EXCLUSIVE_LOCK, 0, 1, 0, overlapped)
	switch {
	case err == nil:
		return file, nil
	case !errors.Is(err, windows.ERROR_IO_PENDING):
		return nil, errors.Join(err, file.Close())
	}
	wait, werr := windows.WaitForSingleObject(event, uint32(d.Milliseconds())) //nolint:gosec // 5000 ms fits uint32
	var done uint32
	switch {
	case werr != nil:
		_ = windows.CancelIoEx(handle, overlapped)
		_ = windows.GetOverlappedResult(handle, overlapped, &done, true)
		return nil, errors.Join(werr, file.Close())
	case wait == windows.WAIT_OBJECT_0:
		if rerr := windows.GetOverlappedResult(handle, overlapped, &done, false); rerr != nil {
			return nil, errors.Join(rerr, file.Close())
		}
		return file, nil
	case wait == uint32(windows.WAIT_TIMEOUT):
		_ = windows.CancelIoEx(handle, overlapped)
		_ = windows.GetOverlappedResult(handle, overlapped, &done, true)
		return nil, errors.Join(errMirrorBusy, file.Close())
	default:
		return nil, errors.Join(fmt.Errorf("journal: wait on %s: status %d", path, wait), file.Close())
	}
}

func releaseGate(file *os.File) error {
	if file == nil {
		return nil
	}
	err := unlockFile(file)
	if errors.Is(err, windows.ERROR_IO_PENDING) {
		err = nil
	}
	return errors.Join(err, file.Close())
}
