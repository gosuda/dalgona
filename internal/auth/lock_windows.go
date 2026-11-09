//go:build windows

package auth

import (
	"errors"
	"fmt"

	"golang.org/x/sys/windows"
)

func LockHome(dir string) (func() error, error) {
	file, err := openAuthLock(dir)
	if err != nil {
		return nil, err
	}
	handle := windows.Handle(file.Fd())
	if err := windows.LockFileEx(handle, windows.LOCKFILE_EXCLUSIVE_LOCK, 0, 1, 0, &windows.Overlapped{}); err != nil {
		return nil, errors.Join(fmt.Errorf("lock auth.lock: %w", err), file.Close())
	}
	return func() error {
		return errors.Join(windows.UnlockFileEx(handle, 0, 1, 0, &windows.Overlapped{}), file.Close())
	}, nil
}
