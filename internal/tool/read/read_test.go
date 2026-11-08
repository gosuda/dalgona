package read

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"io/fs"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func readTestEnv(t *testing.T) (tool.Env, string) {
	t.Helper()
	cwd := t.TempDir()
	local, err := runner.NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	return tool.Env{Runner: local, Cwd: cwd, Home: t.TempDir(), Snapshots: snapshot.New()}, cwd
}

func writeReadFixture(t *testing.T, path, body string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func callRead(t *testing.T, env tool.Env, args string) (tool.Result, error) {
	t.Helper()
	return New().Run(t.Context(), tool.Call{ID: "read", Args: jsontext.Value(args)}, env)
}

func mustRead(t *testing.T, env tool.Env, args string) tool.Result {
	t.Helper()
	result, err := callRead(t, env, args)
	if err != nil {
		t.Fatal(err)
	}
	return result
}

func metadataRef(meta jsontext.Value) string {
	_, rest, _ := strings.Cut(string(meta), `"ref":"`)
	ref, _, _ := strings.Cut(rest, `"`)
	return ref
}

func TestReadReferenceRebuildsFromMeta(t *testing.T) {
	env, cwd := readTestEnv(t)
	body := "one\ntwo\nthree\n"
	writeReadFixture(t, filepath.Join(cwd, "f.txt"), body)
	result := mustRead(t, env, `{"path":"f.txt","offset":2,"limit":1}`)
	if !strings.Contains(string(result.Output), "2:two\n") {
		t.Fatalf("read output %q", result.Output)
	}
	ref := metadataRef(result.Meta)
	rebuilt := snapshot.New()
	if err := rebuilt.Restore(result.Meta, []byte(body)); err != nil {
		t.Fatal(err)
	}
	entry, err := rebuilt.Get(ref)
	if err != nil || string(entry.Data) != body || !rebuilt.Covers(ref, 2, 2) || rebuilt.Covers(ref, 1, 1) {
		t.Fatalf("rebuilt entry %+v: %v", entry, err)
	}
	entry.Data[0] = 'X'
	if again, againErr := rebuilt.Get(ref); againErr != nil || string(again.Data) != body {
		t.Fatal("snapshot buffer is mutable")
	}
}

func TestReadDefaultLimit(t *testing.T) {
	env, cwd := readTestEnv(t)
	var body strings.Builder
	for i := 1; i <= 2005; i++ {
		body.WriteString(strconv.Itoa(i) + "\n")
	}
	writeReadFixture(t, filepath.Join(cwd, "f.txt"), body.String())
	text := string(mustRead(t, env, `{"path":"f.txt"}`).Output)
	if !strings.Contains(text, "1:1\n") || !strings.Contains(text, "2000:2000\n") || strings.Contains(text, "2001:2001\n") {
		t.Fatalf("default window incorrect")
	}
	if !strings.Contains(text, "[Showing lines 1-2000 of 2005. Use 2001 to continue.]") {
		t.Fatalf("missing continuation in %q", text[len(text)-200:])
	}
}

func TestReadAbsoluteSpillAllowlist(t *testing.T) {
	env, _ := readTestEnv(t)
	cases := []struct {
		path    string
		allowed bool
	}{
		{filepath.Join(env.Home, "blobs", strings.Repeat("a", 64)), true},
		{filepath.Join(env.Home, "runs", "call", "out"), true},
		{filepath.Join(env.Home, "sessions", "s1", "runs", "call", "out"), true},
		{filepath.Join(env.Home, "blobs", "short"), false},
		{filepath.Join(env.Home, "auth.json"), false},
		{filepath.Join(t.TempDir(), "outside.bin"), false},
	}
	for _, test := range cases {
		if err := os.MkdirAll(filepath.Dir(test.path), 0o750); err != nil {
			t.Fatal(err)
		}
		writeReadFixture(t, test.path, "spill\n")
		result, err := callRead(t, env, `{"path":`+snapshot.JSONString(test.path)+`}`)
		if test.allowed != (err == nil) || test.allowed && !strings.Contains(string(result.Output), "1:spill\n") {
			t.Fatalf("%s: allowed=%v err=%v", test.path, test.allowed, err)
		}
	}
}

func TestReadTruncatedLineIsNotObserved(t *testing.T) {
	env, cwd := readTestEnv(t)
	body := strings.Repeat("x", 2100) + "\nnext\n"
	writeReadFixture(t, cwd+"/f.txt", body)
	result, err := callRead(t, env, `{"path":"f.txt"}`)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(result.Output), "...") {
		t.Fatal("long line was not cropped")
	}
	ref := metadataRef(result.Meta)
	if env.Snapshots.Covers(ref, 1, 1) || !env.Snapshots.Covers(ref, 2, 2) {
		t.Fatal("cropped line received observation coverage")
	}
}

func TestReadRejectsInvalidRanges(t *testing.T) {
	env, cwd := readTestEnv(t)
	writeReadFixture(t, cwd+"/f.txt", "one\n")
	for _, args := range []string{`{"path":"f.txt","offset":0}`, `{"path":"f.txt","limit":0}`, `{"path":"f.txt","limit":2001}`, `{"path":"f.txt","bogus":1}`} {
		if _, err := callRead(t, env, args); err == nil {
			t.Fatalf("accepted %s", args)
		}
	}
}

type lateSizeRunner struct {
	runner.Runner
}

func (lateSizeRunner) Stat(context.Context, string) (runner.FileInfo, error) {
	return runner.FileInfo{Name: "f.txt", Size: 1, Mode: fs.FileMode(0o644)}, nil
}

func (lateSizeRunner) ReadFile(context.Context, string) ([]byte, error) {
	return bytes.Repeat([]byte{'x'}, maxFileBytes+1), nil
}

func TestReadRechecksSizeAfterRead(t *testing.T) {
	env, _ := readTestEnv(t)
	env.Runner = lateSizeRunner{Runner: env.Runner}
	if _, err := callRead(t, env, `{"path":"f.txt"}`); err == nil || !strings.Contains(err.Error(), "16 MiB") {
		t.Fatalf("late size check error %v", err)
	}
}

func TestReadRejectsUnsafePaths(t *testing.T) {
	env, _ := readTestEnv(t)
	for _, raw := range []string{"../escape", "\x00"} {
		args := `{"path":` + snapshot.JSONString(raw) + `}`
		if _, err := callRead(t, env, args); err == nil {
			t.Fatalf("accepted path %q", raw)
		}
	}
}
