package search

import (
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"
)

const exitQuery = `{"mode":"grep","pattern":"needle","limit":100}`

func builtGenerationArgs(t *testing.T) (ws string, argv []string, dir string) {
	t.Helper()
	requireChangeTimes(t)
	ws = t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"a.txt": "needle\n"})
	root := t.TempDir()
	start := time.Now()
	gen, dir, err := createGeneration(root, start)
	if err != nil {
		t.Fatal(err)
	}
	if err := buildIndex(ws, dir, start, 1<<30); err != nil {
		t.Fatal(err)
	}
	argv = []string{
		"__search", exitQuery, root, gen,
		strconv.FormatInt(start.UnixNano(), 10),
		strconv.FormatInt(racyWindow.Nanoseconds(), 10),
	}
	return ws, argv, dir
}

func TestTrigramSizeMismatchExitsWithStatusOne(t *testing.T) {
	ws, argv, dir := builtGenerationArgs(t)
	t.Chdir(ws)
	trigram := filepath.Join(dir, trigramFile)
	info, err := os.Stat(trigram)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Truncate(trigram, info.Size()-1); err != nil {
		t.Fatal(err)
	}
	code, out, errOut := runIndexedChild(t, argv...)
	if code != indexFailedStatus {
		t.Fatalf("exit code = %d, want %d; stdout %q stderr %q", code, indexFailedStatus, out, errOut)
	}
	if !strings.Contains(out, "trigram size") {
		t.Fatalf("stdout %q lacks trigram size", out)
	}
}

func TestCorruptTrigramExitsWithStatusOne(t *testing.T) {
	ws, argv, dir := builtGenerationArgs(t)
	t.Chdir(ws)
	trigram := filepath.Join(dir, trigramFile)
	info, err := os.Stat(trigram)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(trigram, make([]byte, info.Size()), 0o600); err != nil {
		t.Fatal(err)
	}
	code, out, errOut := runIndexedChild(t, argv...)
	if code != indexFailedStatus {
		t.Fatalf("exit code = %d, want %d; stdout %q stderr %q", code, indexFailedStatus, out, errOut)
	}
	if !strings.Contains(errOut, "corrupt index") {
		t.Fatalf("stderr %q lacks corrupt index", errOut)
	}
}

func TestCorruptManifestExitsWithStatusOne(t *testing.T) {
	ws, argv, dir := builtGenerationArgs(t)
	t.Chdir(ws)
	if err := os.WriteFile(filepath.Join(dir, manifestFile), []byte("junk"), 0o600); err != nil {
		t.Fatal(err)
	}
	code, out, errOut := runIndexedChild(t, argv...)
	if code != indexFailedStatus {
		t.Fatalf("exit code = %d, want %d; stdout %q stderr %q", code, indexFailedStatus, out, errOut)
	}
	if !strings.Contains(out, "invalid manifest") {
		t.Fatalf("stdout %q lacks invalid manifest", out)
	}
}

func TestChildWritesNothingToStderrOnSuccess(t *testing.T) {
	ws, argv, _ := builtGenerationArgs(t)
	t.Chdir(ws)
	code, _, errOut := runIndexedChild(t, argv...)
	if code != 0 {
		t.Fatalf("exit code = %d, want 0; stderr %q", code, errOut)
	}
	if errOut != "" {
		t.Fatalf("a successful search wrote %q to stderr, want nothing", errOut)
	}
}
