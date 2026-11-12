package agent

import (
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestWritableRootsResolveLongestExistingPrefix(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	real := filepath.Join(base, "real")
	if err := os.Mkdir(real, 0o700); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(base, "link")
	if err := os.Symlink(real, link); err != nil {
		t.Skipf("symbolic links unavailable: %v", err)
	}
	realResolved, err := filepath.EvalSymlinks(real)
	if err != nil {
		t.Fatal(err)
	}
	want := filepath.Join(realResolved, "new")
	roots := writableRoots(config.Config{SandboxWritableRoots: []string{filepath.Join(link, "new")}})
	if !slices.Contains(roots, want) {
		t.Fatalf("writable roots %q do not contain %q", roots, want)
	}
}
