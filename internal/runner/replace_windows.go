//go:build windows

package runner

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"syscall"
	"time"

	"golang.org/x/sys/windows"
)

const (
	replaceRetryInterval = 10 * time.Millisecond
	replaceRetryDeadline = time.Second
	replaceKeepLimit     = 4 * 1024
)

func publishReplace(tmp, dst string, perm fs.FileMode) error {
	if replaceUnchanged(tmp, dst, perm) {
		return nil
	}
	cleared := clearReadOnly(dst)
	if err := retryReplaceRename(tmp, dst); err != nil {
		if cleared {
			return errors.Join(err, restoreReadOnly(dst))
		}
		return err
	}
	if cleared {
		return preserveReadOnly(dst, perm)
	}
	return nil
}

func replaceUnchanged(tmp, dst string, perm fs.FileMode) bool {
	if !sameReplaceSize(tmp, dst) {
		return false
	}
	if !replaceModeKept(dst, perm) {
		return false
	}
	return sameReplaceContent(tmp, dst)
}

func sameReplaceSize(tmp, dst string) bool {
	tmpInfo, tmpErr := statNoFollow(tmp)
	if tmpErr != nil || !tmpInfo.Mode().IsRegular() {
		return false
	}
	dstInfo, dstErr := statNoFollow(dst)
	if dstErr != nil || !dstInfo.Mode().IsRegular() {
		return false
	}
	if tmpInfo.Size() != dstInfo.Size() || tmpInfo.Size() > replaceKeepLimit {
		return false
	}
	return true
}

func replaceModeKept(dst string, perm fs.FileMode) bool {
	info, err := statNoFollow(dst)
	if err != nil {
		return false
	}
	return replaceWritable(info.Mode()) == replaceWritable(perm)
}

func replaceWritable(mode fs.FileMode) bool {
	return mode.Perm()&0o200 != 0
}

func sameReplaceContent(tmp, dst string) bool {
	tmpData, tmpErr := os.ReadFile(tmp)
	if tmpErr != nil {
		return false
	}
	dstData, dstErr := os.ReadFile(dst)
	if dstErr != nil {
		return false
	}
	return bytes.Equal(tmpData, dstData)
}

func retryReplaceRename(tmp, dst string) error {
	firstErr := os.Rename(tmp, dst)
	if firstErr == nil || !replaceDenied(firstErr) {
		return firstErr
	}
	if replaceIsDir(dst) {
		return firstErr
	}
	deadline := time.Now().Add(replaceRetryDeadline)
	for time.Now().Before(deadline) {
		time.Sleep(replaceRetryInterval)
		renameErr := os.Rename(tmp, dst)
		if renameErr == nil {
			return nil
		}
		if !replaceDenied(renameErr) {
			return renameErr
		}
	}
	return firstErr
}

func replaceIsDir(dst string) bool {
	info, err := os.Stat(dst)
	return err == nil && info.IsDir()
}

func replaceDenied(err error) bool {
	errno, ok := errors.AsType[syscall.Errno](err)
	if !ok {
		return false
	}
	return errno == syscall.ERROR_ACCESS_DENIED || errno == windows.ERROR_SHARING_VIOLATION
}

const replaceReadOnlyMode = 0o400

func clearReadOnly(dst string) bool {
	info, err := statNoFollow(dst)
	if err != nil || !info.Mode().IsRegular() {
		return false
	}
	if info.Mode().Perm()&0o200 != 0 {
		return false
	}
	return os.Chmod(dst, 0o600) == nil
}

func restoreReadOnly(dst string) error {
	return os.Chmod(dst, replaceReadOnlyMode)
}

func preserveReadOnly(dst string, perm fs.FileMode) error {
	if perm.Perm()&0o200 != 0 {
		return nil
	}
	info, err := statNoFollow(dst)
	if err != nil {
		return fmt.Errorf("runner: stat replaced file: %w", err)
	}
	if info.Mode().Perm()&0o200 == 0 {
		return nil
	}
	if chmodErr := os.Chmod(dst, replaceReadOnlyMode); chmodErr != nil {
		return fmt.Errorf("runner: restore read-only file: %w", chmodErr)
	}
	return nil
}
