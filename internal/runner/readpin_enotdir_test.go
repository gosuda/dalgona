//go:build unix

package runner

import (
	"errors"
	"os"
	"path/filepath"
	"testing"

	"golang.org/x/sys/unix"
)

func TestClassifyPinnedErrorMapsSymlink(t *testing.T) {
	root := t.TempDir()
	target := filepath.Join(root, "target.txt")
	if err := os.WriteFile(target, []byte("data\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(root, "link")
	if err := os.Symlink(target, link); err != nil {
		t.Skipf("symlink creation requires additional privileges: %v", err)
	}
	dir, err := os.Open(root)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = dir.Close() }()
	for _, errno := range []error{unix.ELOOP, unix.ENOTDIR} {
		if err := classifyPinnedError(dir, "link", "link", errno); !errors.Is(err, ErrPinnedSymlink) {
			t.Fatalf("symlink errno %v error = %v, want ErrPinnedSymlink", errno, err)
		}
		if err := classifyPinnedError(dir, "target.txt", "target.txt", errno); errors.Is(err, ErrPinnedSymlink) {
			t.Fatalf("plain file errno %v error = %v, want original error", errno, err)
		}
	}
}
