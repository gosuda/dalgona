//go:build linux

package search

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestBuildNeverReadsOversizedFile(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	path := filepath.Join(ws, "sparse.txt")
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
	m := readBuildManifest(t, buildTestIndex(t, ws, time.Now(), 1<<30))
	if _, ok := m.lookup("sparse.txt"); ok {
		t.Fatal("manifest holds a record for a file over 16 MiB")
	}
	st, ok := fileStat(path, t)
	if !ok {
		t.Fatal("stat sparse file")
	}
	if got := time.Unix(st.Atim.Sec, st.Atim.Nsec); !got.Equal(aged) {
		t.Fatalf("sparse file atime = %v, want %v: the builder read the file", got, aged)
	}
}
