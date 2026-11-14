package search

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"testing"
	"time"
)

func indexedGrep(t *testing.T, ws, dir string, clock freshness, pattern string) outcome {
	t.Helper()
	out, err := execute(ws, args{Mode: "grep", Pattern: pattern, Limit: 100}, dir, clock)
	if err != nil {
		t.Fatal(err)
	}
	return out
}

func clockAt(start time.Time) freshness {
	return freshness{Now: start.UnixNano(), Racy: racyWindow.Nanoseconds()}
}

func requireRow(t *testing.T, text, row string) {
	t.Helper()
	if !strings.Contains(text, row) {
		t.Fatalf("output %q lacks row %q", text, row)
	}
}

func TestCoveredFileWithChangedSizeIsRead(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"a.txt": "xx\n"})
	start := time.Now().Add(time.Hour)
	dir := buildTestIndex(t, ws, start, 1<<30)
	writeBuildFiles(t, ws, map[string]string{"a.txt": "needle here\n"})
	out := indexedGrep(t, ws, dir, clockAt(start), "needle")
	requireRow(t, out.Text, "a.txt:1: needle here")
	if out.Unindexed != 12 {
		t.Fatalf("Unindexed = %d, want 12", out.Unindexed)
	}
}

func TestCoveredFileWithRestoredMtimeIsRead(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	path := filepath.Join(ws, "a.txt")
	writeBuildFiles(t, ws, map[string]string{"a.txt": "xxx\n"})
	before, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	start := time.Now().Add(time.Hour)
	dir := buildTestIndex(t, ws, start, 1<<30)
	writeBuildFiles(t, ws, map[string]string{"a.txt": "yyy\n"})
	if err := os.Chtimes(path, time.Now(), before.ModTime()); err != nil {
		t.Fatal(err)
	}
	out := indexedGrep(t, ws, dir, clockAt(start), "yyy")
	requireRow(t, out.Text, "a.txt:1: yyy")
	if out.Unindexed != 4 {
		t.Fatalf("Unindexed = %d, want 4", out.Unindexed)
	}
}

func TestRacyFileIsReadEvenWhenStatMatches(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"a.txt": "xxx\n"})
	start := time.Now()
	dir := buildTestIndex(t, ws, start, 1<<30)
	out := indexedGrep(t, ws, dir, clockAt(start), "zzz")
	if out.Text != "No matches for zzz" {
		t.Fatalf("Text = %q, want no matches", out.Text)
	}
	if out.Unindexed != 4 {
		t.Fatalf("Unindexed = %d, want 4 for a racy file", out.Unindexed)
	}
}

func TestUncoveredNewFileIsRead(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"a.txt": "xxx\n"})
	start := time.Now().Add(time.Hour)
	dir := buildTestIndex(t, ws, start, 1<<30)
	writeBuildFiles(t, ws, map[string]string{"new.txt": "yyy\n"})
	out := indexedGrep(t, ws, dir, clockAt(start), "yyy")
	requireRow(t, out.Text, "new.txt:1: yyy")
	if out.Unindexed != 4 {
		t.Fatalf("Unindexed = %d, want 4", out.Unindexed)
	}
}

func TestLibraryRejectedFileIsReadNotCounted(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"long.txt": "yyy" + strings.Repeat("x", 1998) + "\n"})
	start := time.Now().Add(time.Hour)
	dir := buildTestIndex(t, ws, start, 1<<30)
	out := indexedGrep(t, ws, dir, clockAt(start), "yyy")
	if !strings.HasPrefix(out.Text, "long.txt:1:") {
		t.Fatalf("Text = %q, want a row for long.txt line 1", out.Text)
	}
	if out.Unindexed != 0 {
		t.Fatalf("Unindexed = %d, want 0", out.Unindexed)
	}
}

func TestChangedRejectedFileIsCounted(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"long.txt": "yyy" + strings.Repeat("x", 1998) + "\n"})
	start := time.Now().Add(time.Hour)
	dir := buildTestIndex(t, ws, start, 1<<30)
	writeBuildFiles(t, ws, map[string]string{"long.txt": "zzz" + strings.Repeat("x", 1998) + "\n"})
	info, err := os.Stat(filepath.Join(ws, "long.txt"))
	if err != nil {
		t.Fatal(err)
	}
	out := indexedGrep(t, ws, dir, clockAt(start), "zzz")
	if out.Unindexed != int(info.Size()) {
		t.Fatalf("Unindexed = %d, want %d for a changed rejected file", out.Unindexed, info.Size())
	}
}

func TestClockBehindStartTrustsNothing(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"a.txt": "xxx\n"})
	start := time.Now().Add(time.Hour)
	dir := buildTestIndex(t, ws, start, 1<<30)
	clock := clockAt(start)
	clock.Now--
	out := indexedGrep(t, ws, dir, clock, "yyy")
	if out.Unindexed != 4 {
		t.Fatalf("Unindexed = %d, want 4 when the clock is behind the build start", out.Unindexed)
	}
}

func TestDeletedGenerationIsNoIndex(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	t.Chdir(ws)
	writeBuildFiles(t, ws, map[string]string{"a.txt": "needle\n"})
	root := t.TempDir()
	start := time.Now()
	gen, dir, err := createGeneration(root, start)
	if err != nil {
		t.Fatal(err)
	}
	err = buildIndex(ws, dir, start, 1<<30)
	if err != nil {
		t.Fatal(err)
	}
	err = os.RemoveAll(dir)
	if err != nil {
		t.Fatal(err)
	}
	const query = `{"mode":"grep","pattern":"needle","limit":100}`
	var stdout bytes.Buffer
	now := strconv.FormatInt(start.UnixNano(), 10)
	racy := strconv.FormatInt(racyWindow.Nanoseconds(), 10)
	if code := Main([]string{query, root, gen, now, racy}, &stdout); code != 0 {
		t.Fatalf("Main returned %d, want 0 for a deleted generation", code)
	}
	parsed, err := parseArgs(jsontext.Value(query))
	if err != nil {
		t.Fatal(err)
	}
	want, err := execute(ws, parsed, "", freshness{})
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(stdout.Bytes(), encodeOutcome(want)) {
		t.Fatalf("Main output differs from the scan:\n%s\nwant:\n%s", stdout.Bytes(), encodeOutcome(want))
	}
}

func TestFindIgnoresGeneration(t *testing.T) {
	requireChangeTimes(t)
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"a.txt": "xxx\n", "b.md": "yyy\n"})
	dir := buildTestIndex(t, ws, time.Now(), 1<<30)
	with, err := execute(ws, args{Mode: "find", Pattern: "*.txt", Limit: 100}, dir, clockAt(time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	without, err := execute(ws, args{Mode: "find", Pattern: "*.txt", Limit: 100}, "", freshness{})
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(with, without) {
		t.Fatalf("find with a generation = %+v, without = %+v", with, without)
	}
}

func TestUnindexedCountsEveryUncoveredRead(t *testing.T) {
	ws := t.TempDir()
	writeBuildFiles(t, ws, map[string]string{"a.txt": "xxxx", "b.txt": "yyyyyy"})
	out, err := execute(ws, args{Mode: "grep", Pattern: "zzz", Limit: 100}, "", freshness{})
	if err != nil {
		t.Fatal(err)
	}
	if out.Unindexed != 10 {
		t.Fatalf("Unindexed = %d, want 10 without an index", out.Unindexed)
	}
}

func TestMainRejectsGenerationOutsideRoot(t *testing.T) {
	var stdout bytes.Buffer
	if code := Main([]string{`{"mode":"grep","pattern":"x"}`, t.TempDir(), "../x", "1", "2"}, &stdout); code != 2 {
		t.Fatalf("Main returned %d, want 2 for generation ../x", code)
	}
}

func testExecutable(tb testing.TB) string {
	tb.Helper()
	exe, err := os.Executable()
	if err != nil {
		tb.Fatal(err)
	}
	resolved, err := filepath.EvalSymlinks(exe)
	if err != nil {
		tb.Fatal(err)
	}
	abs, err := filepath.Abs(resolved)
	if err != nil {
		tb.Fatal(err)
	}
	return abs
}

func runIndexedChild(t *testing.T, args ...string) (code int, stdout, stderr string) {
	t.Helper()
	var outBuf, errBuf bytes.Buffer
	cmd := exec.Command(testExecutable(t), args...)
	cmd.Stdout = &outBuf
	cmd.Stderr = &errBuf
	err := cmd.Run()
	if err == nil {
		return 0, outBuf.String(), errBuf.String()
	}
	exitErr, ok := errors.AsType[*exec.ExitError](err)
	if !ok {
		t.Fatal(err)
	}
	return exitErr.ExitCode(), outBuf.String(), errBuf.String()
}
