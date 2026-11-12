//go:build linux

package search

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestSettledUnnamedFileIsNotRead(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	path := filepath.Join(ws, "a.txt")
	writeBuildFiles(t, ws, map[string]string{"a.txt": "xxx\n"})
	mtime := time.Now().Add(-time.Hour).Truncate(time.Second)
	aged := time.Now().Add(-48 * time.Hour).Truncate(time.Second)
	if err := os.Chtimes(path, aged, mtime); err != nil {
		t.Fatal(err)
	}
	start := time.Now().Add(time.Hour)
	dir := buildTestIndex(t, ws, start, 1<<30)
	atime := fileAtime(t, path)
	out := indexedGrep(t, ws, dir, clockAt(start), "yyy")
	if out.Text != "No matches for yyy" {
		t.Fatalf("Text = %q, want no matches", out.Text)
	}
	if out.Unindexed != 0 {
		t.Fatalf("Unindexed = %d, want 0", out.Unindexed)
	}
	if got := fileAtime(t, path); !got.Equal(atime) {
		t.Fatalf("atime moved from %v to %v: the settled file was read", atime, got)
	}
}

func fileAtime(t *testing.T, path string) time.Time {
	t.Helper()
	st, ok := fileStat(path, t)
	if !ok {
		t.Fatal("stat file")
	}
	return time.Unix(st.Atim.Sec, st.Atim.Nsec)
}
