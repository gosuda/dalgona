//go:build linux

package search

import (
	"os"
	"path/filepath"
	"strings"
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

func ageAccess(t *testing.T, ws, name string) time.Time {
	t.Helper()
	aged := time.Now().Add(-48 * time.Hour).Truncate(time.Second)
	if err := os.Chtimes(filepath.Join(ws, name), aged, time.Now()); err != nil {
		t.Fatal(err)
	}
	return aged
}

func accessTime(t *testing.T, ws, name string) time.Time {
	t.Helper()
	st, ok := fileStat(filepath.Join(ws, name), t)
	if !ok {
		t.Fatalf("stat %s", name)
	}
	return time.Unix(st.Atim.Sec, st.Atim.Nsec)
}

func TestBuildReadsNoFileAfterTheLimitIsPassed(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{
		"a.txt": "012345678\n",
		"b.txt": strings.Repeat("x", 36) + "\n",
		"c.txt": "yy\n",
		"d.txt": "zz\n",
	})
	aged := make(map[string]time.Time)
	for _, name := range []string{"a.txt", "b.txt", "c.txt", "d.txt"} {
		aged[name] = ageAccess(t, ws, name)
	}
	m := readBuildManifest(t, buildTestIndex(t, ws, time.Now(), 20))
	if rec, _ := m.lookup("a.txt"); !rec.Indexed {
		t.Fatal("a.txt is not indexed, want the file inside the limit read")
	}
	if accessTime(t, ws, "a.txt").Equal(aged["a.txt"]) {
		t.Skip("the file system does not update access times on read")
	}
	for _, name := range []string{"b.txt", "c.txt", "d.txt"} {
		if got := accessTime(t, ws, name); !got.Equal(aged[name]) {
			t.Fatalf("%s atime = %v, want %v: the builder read a file after the limit was passed", name, got, aged[name])
		}
	}
}
