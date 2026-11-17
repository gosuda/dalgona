package search

import (
	"os"
	"path/filepath"
	"slices"
	"testing"
)

func TestFilesReturnsFirstPathsUpToLimit(t *testing.T) {
	root := t.TempDir()
	for _, name := range []string{"a.go", "b.go", "c.go", "d.go", "e.go"} {
		if err := os.WriteFile(filepath.Join(root, name), []byte("x"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	paths, err := Files(root, 2)
	if err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(paths, []string{"a.go", "b.go"}) {
		t.Fatalf("Files(limit 2) = %v, want [a.go b.go]", paths)
	}
}
