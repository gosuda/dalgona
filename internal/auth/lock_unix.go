//go:build !windows

package auth

import (
	"errors"
	"fmt"

	"golang.org/x/sys/unix"
)

func lockAuth(dir string) (func() error, error) {
	file, err := openAuthLock(dir)
	if err != nil {
		return nil, err
	}
	if err := unix.Flock(int(file.Fd()), unix.LOCK_EX); err != nil {
		return nil, errors.Join(fmt.Errorf("lock auth.lock: %w", err), file.Close())
	}
	return func() error {
		return errors.Join(unix.Flock(int(file.Fd()), unix.LOCK_UN), file.Close())
	}, nil
}
