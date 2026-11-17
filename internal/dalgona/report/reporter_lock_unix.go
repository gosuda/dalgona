//go:build !windows

package report

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"

	"golang.org/x/sys/unix"
)

func lockReporterDir(dir string) (func() error, error) {
	file, err := os.OpenFile(filepath.Join(dir, "report.lock"), os.O_RDWR|os.O_CREATE, 0o600)
	if err != nil {
		return nil, fmt.Errorf("failure reporting lock local state: %w", err)
	}
	if err := unix.Flock(int(file.Fd()), unix.LOCK_EX|unix.LOCK_NB); err != nil {
		_ = file.Close()
		return nil, errors.New("failure reporting is already running")
	}
	return func() error {
		return errors.Join(unix.Flock(int(file.Fd()), unix.LOCK_UN), file.Close())
	}, nil
}
