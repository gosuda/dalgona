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
	file, err := openGateFile(path)
	if err != nil {
		return nil, err
	}
	if err := waitGate(path, windows.Handle(file.Fd()), d); err != nil {
		return nil, errors.Join(err, file.Close())
	}
	return file, nil
}

func openGateFile(path string) (*os.File, error) {
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
	return file, nil
}

func waitGate(path string, handle windows.Handle, d time.Duration) error {
	event, err := windows.CreateEvent(nil, 1, 0, nil)
	if err != nil {
		return err
	}
	defer func() { _ = windows.CloseHandle(event) }()
	overlapped := &windows.Overlapped{HEvent: event}
	err = windows.LockFileEx(handle, windows.LOCKFILE_EXCLUSIVE_LOCK, 0, 1, 0, overlapped)
	if err == nil {
		return nil
	}
	if !errors.Is(err, windows.ERROR_IO_PENDING) {
		return err
	}
	wait, werr := windows.WaitForSingleObject(event, uint32(d.Milliseconds())) //nolint:gosec // 5000 ms fits uint32
	var done uint32
	switch {
	case werr != nil:
		return errors.Join(werr, cancelGateWait(handle, overlapped))
	case wait == windows.WAIT_OBJECT_0:
		return windows.GetOverlappedResult(handle, overlapped, &done, false)
	case wait == uint32(windows.WAIT_TIMEOUT):
		return errors.Join(errMirrorBusy, cancelGateWait(handle, overlapped))
	default:
		return errors.Join(fmt.Errorf("journal: wait on %s: status %d", path, wait), cancelGateWait(handle, overlapped))
	}
}

func cancelGateWait(handle windows.Handle, overlapped *windows.Overlapped) error {
	_ = windows.CancelIoEx(handle, overlapped)
	var done uint32
	return windows.GetOverlappedResult(handle, overlapped, &done, true)
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
