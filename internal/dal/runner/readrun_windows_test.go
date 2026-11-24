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

func TestReadRunFileStopsAtDeadline(t *testing.T) {
	cases := []struct {
		name      string
		secondEnd time.Duration
		wantFail  time.Duration
	}{
		{"second read ends before the deadline", replaceRetryDeadline - replaceRetryInterval/2, replaceRetryDeadline},
		{"second read ends after the deadline", replaceRetryDeadline + replaceRetryInterval/2, replaceRetryDeadline + replaceRetryInterval/2},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) { checkReadRunFileStopsAtDeadline(t, tc.secondEnd, tc.wantFail) })
		})
	}
}

func checkReadRunFileStopsAtDeadline(t *testing.T, secondEnd, wantFail time.Duration) {
	t.Helper()
	held := &fs.PathError{Op: "open", Path: "pid", Err: windows.ERROR_SHARING_VIOLATION}
	attempts := 0
	var firstDone time.Time
	useRunFileRead(t, func(string) ([]byte, error) {
		attempts++
		switch attempts {
		case 1:
			firstDone = time.Now()
		case 2:
			time.Sleep(time.Until(firstDone.Add(secondEnd)))
		}
		return nil, held
	})
	_, err := readRunFile("pid")
	failedAt := time.Since(firstDone)
	if !errors.Is(err, windows.ERROR_SHARING_VIOLATION) {
		t.Fatalf("read held past the deadline: %v, want sharing violation", err)
	}
	if attempts != 2 {
		t.Fatalf("read attempts = %d, want 2: no attempt may start after the deadline", attempts)
	}
	if failedAt != wantFail {
		t.Fatalf("read failed %v after the first attempt, want %v", failedAt, wantFail)
	}
}
