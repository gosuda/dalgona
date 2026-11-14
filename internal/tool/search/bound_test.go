package search

import (
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
)

func TestScanFileSkipsSparseHugeFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "sparse.txt")
	out, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := out.WriteString("hit\n"); err != nil {
		t.Fatal(err)
	}
	if err := out.Truncate(64 << 20); err != nil {
		t.Fatal(err)
	}
	if err := out.Close(); err != nil {
		t.Fatal(err)
	}
	entry := fileEntry{absolute: path, display: "sparse.txt", searchRel: "sparse.txt"}
	file, matched := scanFile(entry, regexp.MustCompile("hit"))
	if len(matched) != 0 || len(file.data) != 0 {
		t.Fatalf("sparse huge file matched = %v data %d bytes, want skipped", matched, len(file.data))
	}
}

func TestBoundMainSearchCeiling(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	data := []byte("hit\n" + strings.Repeat("x\n", ((16<<20)-4)/2))
	path := filepath.Join(root, "boundary.txt")
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	result := runMain(t, `{"mode":"grep","pattern":"hit","path":"boundary.txt"}`)
	if !strings.HasPrefix(result.Text, "boundary.txt:1: hit\n") {
		t.Fatalf("exact ceiling output = %q, want first-line match", result.Text)
	}
	if err := os.WriteFile(path, append(data, '\n'), 0o600); err != nil {
		t.Fatal(err)
	}
	result = runMain(t, `{"mode":"grep","pattern":"hit","path":"boundary.txt"}`)
	if result.Text != "No matches for hit" {
		t.Fatalf("over ceiling output = %q, want no matches", result.Text)
	}
}
