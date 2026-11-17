//go:build windows

package report

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"

	"golang.org/x/sys/windows"
)

func lockReporterDir(dir string) (func() error, error) {
	file, err := os.OpenFile(filepath.Join(dir, "report.lock"), os.O_RDWR|os.O_CREATE, 0o600)
	if err != nil {
		return nil, fmt.Errorf("failure reporting lock local state: %w", err)
	}
	handle := windows.Handle(file.Fd())
	var overlapped windows.Overlapped
	if err := windows.LockFileEx(handle, windows.LOCKFILE_EXCLUSIVE_LOCK|windows.LOCKFILE_FAIL_IMMEDIATELY, 0, 1, 0, &overlapped); err != nil {
		_ = file.Close()
		return nil, errors.New("failure reporting is already running")
	}
	return func() error {
		return errors.Join(windows.UnlockFileEx(handle, 0, 1, 0, &overlapped), file.Close())
	}, nil
}
