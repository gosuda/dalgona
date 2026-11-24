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

func TestRetryRootRenameStartsNoRetryPastDeadline(t *testing.T) {
	root, openErr := os.OpenRoot(t.TempDir())
	if openErr != nil {
		t.Fatal(openErr)
	}
	t.Cleanup(func() { _ = root.Close() })
	synctest.Test(t, func(t *testing.T) {
		held := &os.LinkError{Op: "renameat", Old: "f.txt.tmp", New: "f.txt", Err: windows.ERROR_SHARING_VIOLATION}
		attempts := 0
		var firstDone time.Time
		useRootRename(t, func(*os.Root, string, string) error {
			attempts++
			switch attempts {
			case 1:
				firstDone = time.Now()
			case 2:
				time.Sleep(time.Until(firstDone.Add(replaceRetryDeadline - replaceRetryInterval/2)))
			}
			return held
		})
		if err := retryRootRename(root, "f.txt.tmp", "f.txt"); !errors.Is(err, windows.ERROR_SHARING_VIOLATION) {
			t.Fatalf("replace held past the deadline: %v, want sharing violation", err)
		}
		if attempts != 2 {
			t.Fatalf("rename attempts = %d, want 2: no attempt may start after the deadline", attempts)
		}
	})
}
