//go:build windows

package runner

import (
	"errors"
	"io/fs"
	"os"
	"testing"
	"testing/synctest"
	"time"

	"golang.org/x/sys/windows"
)

func useRunFileRead(t *testing.T, read func(name string) ([]byte, error)) {
	t.Helper()
	runFileRead = read
	t.Cleanup(func() { runFileRead = os.ReadFile })
}

func TestReadRunFileStartsNoRetryPastDeadline(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		held := &fs.PathError{Op: "open", Path: "pid", Err: windows.ERROR_SHARING_VIOLATION}
		attempts := 0
		var firstDone time.Time
		useRunFileRead(t, func(string) ([]byte, error) {
			attempts++
			switch attempts {
			case 1:
				firstDone = time.Now()
			case 2:
				time.Sleep(time.Until(firstDone.Add(replaceRetryDeadline - replaceRetryInterval/2)))
			}
			return nil, held
		})
		data, err := readRunFile("pid")
		if !errors.Is(err, windows.ERROR_SHARING_VIOLATION) {
			t.Fatalf("read held past the deadline: %v, want sharing violation", err)
		}
		if data != nil {
			t.Fatalf("read held past the deadline returned %q, want no data", data)
		}
		if attempts != 2 {
			t.Fatalf("read attempts = %d, want 2: no attempt may start after the deadline", attempts)
		}
	})
}
