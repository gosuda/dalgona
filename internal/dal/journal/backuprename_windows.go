//go:build windows

package journal

import (
	"errors"
	"os"
	"syscall"
	"time"

	"golang.org/x/sys/windows"
)

const (
	backupRenameRetryInterval = 10 * time.Millisecond
	backupRenameRetryDeadline = time.Second
)

func renameBackupDir(oldpath, newpath string) error {
	firstErr := os.Rename(oldpath, newpath)
	if firstErr == nil || !backupRenameDenied(firstErr) {
		return firstErr
	}
	deadline := time.Now().Add(backupRenameRetryDeadline)
	for time.Now().Before(deadline) {
		time.Sleep(backupRenameRetryInterval)
		renameErr := os.Rename(oldpath, newpath)
		if renameErr == nil || !backupRenameDenied(renameErr) {
			return renameErr
		}
	}
	return firstErr
}

func backupRenameDenied(err error) bool {
	errno, ok := errors.AsType[syscall.Errno](err)
	if !ok {
		return false
	}
	return errno == syscall.ERROR_ACCESS_DENIED || errno == windows.ERROR_SHARING_VIOLATION
}
