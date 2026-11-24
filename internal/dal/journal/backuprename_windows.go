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

var backupRename = os.Rename

func renameBackupDir(oldpath, newpath string) error {
	firstErr := backupRename(oldpath, newpath)
	if firstErr == nil || !backupRenameDenied(firstErr) {
		return firstErr
	}
	deadline := time.Now().Add(backupRenameRetryDeadline)
	for {
		time.Sleep(backupRenameRetryInterval)
		if !time.Now().Before(deadline) {
			return firstErr
		}
		renameErr := backupRename(oldpath, newpath)
		if renameErr == nil || !backupRenameDenied(renameErr) {
			return renameErr
		}
	}
}

func backupRenameDenied(err error) bool {
	errno, ok := errors.AsType[syscall.Errno](err)
	if !ok {
		return false
	}
	return errno == syscall.ERROR_ACCESS_DENIED || errno == windows.ERROR_SHARING_VIOLATION
}
