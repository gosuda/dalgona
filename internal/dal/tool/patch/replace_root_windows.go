//go:build windows

package patch

import (
	"errors"
	"os"
	"syscall"
	"time"

	"golang.org/x/sys/windows"
)

const (
	replaceRetryInterval = 10 * time.Millisecond
	replaceRetryDeadline = time.Second
	replaceReadOnlyMode  = 0o400
)

func replaceInRoot(root *os.Root, oldName, newName string) error {
	cleared := clearReadOnlyInRoot(root, newName)
	if err := retryRootRename(root, oldName, newName); err != nil {
		if cleared {
			return errors.Join(err, root.Chmod(newName, replaceReadOnlyMode))
		}
		return err
	}
	return nil
}

func clearReadOnlyInRoot(root *os.Root, name string) bool {
	info, err := root.Lstat(name)
	if err != nil || !info.Mode().IsRegular() || info.Mode().Perm()&0o200 != 0 {
		return false
	}
	return root.Chmod(name, 0o600) == nil
}

func retryRootRename(root *os.Root, oldName, newName string) error {
	firstErr := root.Rename(oldName, newName)
	if firstErr == nil || !rootReplaceDenied(firstErr) {
		return firstErr
	}
	if info, err := root.Lstat(newName); err == nil && info.IsDir() {
		return firstErr
	}
	deadline := time.Now().Add(replaceRetryDeadline)
	for time.Now().Before(deadline) {
		time.Sleep(replaceRetryInterval)
		renameErr := root.Rename(oldName, newName)
		if renameErr == nil {
			return nil
		}
		if !rootReplaceDenied(renameErr) {
			return renameErr
		}
	}
	return firstErr
}

func rootReplaceDenied(err error) bool {
	errno, ok := errors.AsType[syscall.Errno](err)
	if !ok {
		return false
	}
	return errno == syscall.ERROR_ACCESS_DENIED || errno == windows.ERROR_SHARING_VIOLATION
}
