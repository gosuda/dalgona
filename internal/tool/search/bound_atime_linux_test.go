//go:build linux

package search

import (
	"os"
	"path/filepath"
	"regexp"
	"syscall"
	"testing"
	"time"
)

func TestScanFileDoesNotReadHugeFile(t *testing.T) {
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
	aged := time.Now().Add(-48 * time.Hour).Truncate(time.Second)
	if err := os.Chtimes(path, aged, time.Now()); err != nil {
		t.Fatal(err)
	}
	entry := fileEntry{absolute: path, display: "sparse.txt", searchRel: "sparse.txt"}
	if _, matched := scanFile(entry, regexp.MustCompile("hit")); len(matched) != 0 {
		t.Fatalf("huge file matched = %v, want skipped", matched)
	}
	st, ok := fileStat(path, t)
	if !ok {
		t.Fatal("stat sparse file")
	}
	if got := time.Unix(st.Atim.Sec, st.Atim.Nsec); !got.Equal(aged) {
		t.Fatalf("huge file atime = %v, want %v: file was read", got, aged)
	}
}

func fileStat(path string, t *testing.T) (*syscall.Stat_t, bool) {
	t.Helper()
	info, err := os.Stat(path)
	if err != nil {
		return nil, false
	}
	st, ok := info.Sys().(*syscall.Stat_t)
	return st, ok
}
