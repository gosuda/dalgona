//go:build windows

package runner

import (
	"errors"
	"os"
	"syscall"
	"time"

	"golang.org/x/sys/windows"
)

var runFileRead = os.ReadFile

func readRunFile(path string) ([]byte, error) {
	deadline := time.Now().Add(replaceRetryDeadline)
	for {
		data, err := runFileRead(path)
		if err == nil {
			return data, nil
		}
		if !runFileSharingViolation(err) {
			return nil, err
		}
		time.Sleep(min(replaceRetryInterval, time.Until(deadline)))
		if !time.Now().Before(deadline) {
			return nil, err
		}
	}
}

func runFileSharingViolation(err error) bool {
	errno, ok := errors.AsType[syscall.Errno](err)
	return ok && errno == windows.ERROR_SHARING_VIOLATION
}
