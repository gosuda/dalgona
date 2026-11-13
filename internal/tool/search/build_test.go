package search

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

func writeBuildFiles(t *testing.T, root string, files map[string]string) {
	t.Helper()
	for name, body := range files {
		path := filepath.Join(root, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
}

func buildTestIndex(t *testing.T, ws string, start time.Time, limit int64) string {
	t.Helper()
	_, dir, err := createGeneration(t.TempDir(), start)
	if err != nil {
		t.Fatal(err)
	}
	if err := buildIndex(ws, dir, start, limit); err != nil {
		t.Fatal(err)
	}
	return dir
}

func readBuildManifest(t *testing.T, dir string) manifest {
	t.Helper()
	m, err := readManifest(filepath.Join(dir, manifestFile))
	if err != nil {
		t.Fatal(err)
	}
	return m
}

func requireChangeTimes(t testing.TB) {
	t.Helper()
	if !ctimeSupported {
		t.Skip("the index needs change times on this platform")
	}
}

func TestBuildWritesManifestAndTrigram(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"src/a.go": "needle one\n", "b.txt": "needle two\n"})
	dir := buildTestIndex(t, ws, time.Now(), 1<<30)
	m := readBuildManifest(t, dir)
	if len(m.Entries) != 2 || m.Entries[0].Path != "b.txt" || m.Entries[1].Path != "src/a.go" {
		t.Fatalf("manifest paths = %+v, want b.txt then src/a.go", m.Entries)
	}
	for _, rec := range m.Entries {
		if rec.Size != 11 || !rec.Indexed {
			t.Fatalf("record %s = %+v, want size 11 and indexed", rec.Path, rec)
		}
		requireRecordMatchesFile(t, ws, rec)
	}
	trigram, err := os.Stat(filepath.Join(dir, trigramFile))
	if err != nil {
		t.Fatal(err)
	}
	if m.TrigramSize != trigram.Size() {
		t.Fatalf("trigram size %d, want %d", m.TrigramSize, trigram.Size())
	}
}

func requireRecordMatchesFile(t *testing.T, ws string, rec manifestEntry) {
	t.Helper()
	info, err := os.Stat(filepath.Join(ws, filepath.FromSlash(rec.Path)))
	if err != nil {
		t.Fatal(err)
	}
	if rec.ModNS != info.ModTime().UnixNano() {
		t.Fatalf("record %s mtime %d, want %d", rec.Path, rec.ModNS, info.ModTime().UnixNano())
	}
	ctime, ok := ctimeNS(info)
	if !ok || rec.CtimeNS != ctime {
		t.Fatalf("record %s ctime %d, want %d", rec.Path, rec.CtimeNS, ctime)
	}
}

func TestBuildMarksLibraryRejectedFilesUnindexed(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{
		"long.txt": strings.Repeat("x", 2001) + "\n",
		"ok.txt":   "needle\n",
	})
	m := readBuildManifest(t, buildTestIndex(t, ws, time.Now(), 1<<30))
	long, ok := m.lookup("long.txt")
	if !ok || long.Indexed || long.Size != 2002 {
		t.Fatalf("long.txt record = %+v, %v; want size 2002 and not indexed", long, ok)
	}
	ok2, found := m.lookup("ok.txt")
	if !found || !ok2.Indexed {
		t.Fatalf("ok.txt record = %+v, %v; want indexed", ok2, found)
	}
}

func TestBuildRecordsUnreadableTextUnindexed(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"bin.dat": "a\x00b\n"})
	m := readBuildManifest(t, buildTestIndex(t, ws, time.Now(), 1<<30))
	rec, ok := m.lookup("bin.dat")
	if !ok || rec.Indexed {
		t.Fatalf("bin.dat record = %+v, %v; want a record that is not indexed", rec, ok)
	}
}

func TestBuildSkipsSymbolicLinks(t *testing.T) {
	requireChangeTimes(t)
	if runtime.GOOS != "linux" && runtime.GOOS != "darwin" {
		t.Skip("symbolic links need a platform with change times")
	}
	ws := t.TempDir()
	outside := filepath.Join(t.TempDir(), "outside.txt")
	if err := os.WriteFile(outside, []byte("needle\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(outside, filepath.Join(ws, "link.txt")); err != nil {
		t.Fatal(err)
	}
	writeBuildFiles(t, ws, map[string]string{"real.txt": "needle\n"})
	m := readBuildManifest(t, buildTestIndex(t, ws, time.Now(), 1<<30))
	if _, ok := m.lookup("link.txt"); ok {
		t.Fatal("manifest holds a record for the symbolic link link.txt")
	}
	if _, ok := m.lookup("real.txt"); !ok {
		t.Fatal("manifest lacks real.txt")
	}
}

func TestBuildRespectsByteLimit(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{
		"a.txt": "xxxxxxxxxx\n",
		"b.txt": "yyyyyyyyyy\n",
	})
	m := readBuildManifest(t, buildTestIndex(t, ws, time.Now(), 20))
	a, okA := m.lookup("a.txt")
	b, okB := m.lookup("b.txt")
	if !okA || !okB {
		t.Fatalf("records a.txt %v, b.txt %v; want both recorded", okA, okB)
	}
	if !a.Indexed {
		t.Fatal("a.txt is not indexed, want indexed within the limit")
	}
	if b.Indexed {
		t.Fatal("b.txt is indexed, want it left unread past the limit")
	}
}

func TestBuildKeepsReadingPastAFileOverTheLimit(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{
		"a.txt": strings.Repeat("x", 36) + "\n",
		"b.txt": "yy\n",
	})
	m := readBuildManifest(t, buildTestIndex(t, ws, time.Now(), 20))
	a, okA := m.lookup("a.txt")
	b, okB := m.lookup("b.txt")
	if !okA || !okB {
		t.Fatalf("records a.txt %v, b.txt %v; want both recorded", okA, okB)
	}
	if a.Indexed {
		t.Fatal("a.txt is indexed, want it left unread past the limit")
	}
	if !b.Indexed {
		t.Fatal("b.txt is not indexed, want it read after the file over the limit was skipped")
	}
}

func TestPublishLaterSwitchWins(t *testing.T) {
	root := t.TempDir()
	gen1, dir1, err := createGeneration(root, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	gen2, dir2, err := createGeneration(root, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	for _, gen := range []string{gen1, gen2} {
		if err = publishGeneration(root, gen); err != nil {
			t.Fatal(err)
		}
	}
	requireCurrent(t, root, gen2)
	if err = removeOtherGenerations(root, gen2); err != nil {
		t.Fatal(err)
	}
	if _, err = os.Stat(dir1); !os.IsNotExist(err) {
		t.Fatalf("generation %s still exists: %v", gen1, err)
	}
	if _, err = os.Stat(dir2); err != nil {
		t.Fatalf("published generation %s was removed: %v", gen2, err)
	}
}

func requireCurrent(t *testing.T, root, gen string) {
	t.Helper()
	current, err := os.ReadFile(filepath.Join(root, currentFile))
	if err != nil {
		t.Fatal(err)
	}
	if string(current) != gen+"\n" {
		t.Fatalf("CURRENT = %q, want %q", current, gen+"\n")
	}
}

func TestGenerationNamesDiffer(t *testing.T) {
	start := time.Unix(1700000000, 5)
	first, err := generationName(start)
	if err != nil {
		t.Fatal(err)
	}
	second, err := generationName(start)
	if err != nil {
		t.Fatal(err)
	}
	if first == second {
		t.Fatalf("generation names are equal: %s", first)
	}
	prefix := fmt.Sprintf("%020d-", start.UnixNano())
	if !strings.HasPrefix(first, prefix) || !strings.HasPrefix(second, prefix) {
		t.Fatalf("names %s and %s do not begin with %s", first, second, prefix)
	}
}

func TestBuildMainArguments(t *testing.T) {
	var out bytes.Buffer
	if got := BuildMain([]string{"a", "b"}, &out); got != 2 {
		t.Fatalf("two arguments returned %d, want 2", got)
	}
	if !strings.Contains(out.String(), "usage: __search-index <root> <generation> <start-ns>") {
		t.Fatalf("usage line missing: %q", out.String())
	}
	out.Reset()
	if got := BuildMain([]string{t.TempDir(), "../x", "1"}, &out); got != 2 {
		t.Fatalf("generation ../x returned %d, want 2", got)
	}
}

func TestIndexedNamesReportsMissingTrigram(t *testing.T) {
	_, err := indexedNames(t.TempDir())
	if !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("indexedNames without a trigram file: err = %v, want fs.ErrNotExist", err)
	}
}
