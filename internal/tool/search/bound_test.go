package search

import (
	"os"
	"path/filepath"
	"regexp"
	"testing"
)

func TestScanFileSkipsOversizedFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "big.txt")
	chunk := make([]byte, 1<<20)
	for i := range chunk {
		chunk[i] = 'x'
	}
	chunk[len(chunk)-1] = '\n'
	out, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	for range 17 {
		if _, err := out.Write(chunk); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := out.WriteString("hit\n"); err != nil {
		t.Fatal(err)
	}
	if err := out.Close(); err != nil {
		t.Fatal(err)
	}
	entry := fileEntry{absolute: path, display: "big.txt", searchRel: "big.txt"}
	file, matched := scanFile(entry, regexp.MustCompile("hit"))
	if len(matched) != 0 || len(file.data) != 0 {
		t.Fatalf("oversized file matched = %v data %d bytes, want skipped", matched, len(file.data))
	}
}

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
