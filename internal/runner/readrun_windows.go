//go:build windows

package runner

import (
	"errors"
	"os"
	"syscall"
	"time"

	"golang.org/x/sys/windows"
)

func readRunFile(path string) ([]byte, error) {
	deadline := time.Now().Add(replaceRetryDeadline)
	for {
		data, err := os.ReadFile(path)
		if err == nil {
			return data, nil
		}
		if !runFileSharingViolation(err) || !time.Now().Before(deadline) {
			return nil, err
		}
		time.Sleep(replaceRetryInterval)
	}
}

func runFileSharingViolation(err error) bool {
	errno, ok := errors.AsType[syscall.Errno](err)
	return ok && errno == windows.ERROR_SHARING_VIOLATION
}
