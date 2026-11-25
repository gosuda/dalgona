//go:build windows

package patch

import (
	"errors"
	"os"
	"testing"
	"testing/synctest"
	"time"

	"golang.org/x/sys/windows"
)

func useRootRename(t *testing.T, rename func(root *os.Root, oldName, newName string) error) {
	t.Helper()
	rootRename = rename
	t.Cleanup(func() { rootRename = (*os.Root).Rename })
}

func TestRetryRootRenameStopsAtDeadline(t *testing.T) {
	root, openErr := os.OpenRoot(t.TempDir())
	if openErr != nil {
		t.Fatal(openErr)
	}
	t.Cleanup(func() { _ = root.Close() })
	cases := []struct {
		name      string
		secondEnd time.Duration
		wantFail  time.Duration
	}{
		{"second attempt ends before the deadline", replaceRetryDeadline - replaceRetryInterval/2, replaceRetryDeadline},
		{"second attempt ends after the deadline", replaceRetryDeadline + replaceRetryInterval/2, replaceRetryDeadline + replaceRetryInterval/2},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) { checkRootRenameStopsAtDeadline(t, root, tc.secondEnd, tc.wantFail) })
		})
	}
}

func checkRootRenameStopsAtDeadline(t *testing.T, root *os.Root, secondEnd, wantFail time.Duration) {
	t.Helper()
	held := &os.LinkError{Op: "renameat", Old: "f.txt.tmp", New: "f.txt", Err: windows.ERROR_SHARING_VIOLATION}
	attempts := 0
	var firstDone time.Time
	useRootRename(t, func(*os.Root, string, string) error {
		attempts++
		switch attempts {
		case 1:
			firstDone = time.Now()
		case 2:
			time.Sleep(time.Until(firstDone.Add(secondEnd)))
		}
		return held
	})
	err := retryRootRename(root, "f.txt.tmp", "f.txt")
	failedAt := time.Since(firstDone)
	if !errors.Is(err, windows.ERROR_SHARING_VIOLATION) {
		t.Fatalf("replace held past the deadline: %v, want sharing violation", err)
	}
	if attempts != 2 {
		t.Fatalf("rename attempts = %d, want 2: no attempt may start after the deadline", attempts)
	}
	if failedAt != wantFail {
		t.Fatalf("replace failed %v after the first attempt, want %v", failedAt, wantFail)
	}
}
