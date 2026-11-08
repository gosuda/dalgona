package search

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func writeSearchFixture(t *testing.T, root, name, body string) {
	t.Helper()
	path := filepath.Join(root, name)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func runMain(t *testing.T, args string) outcome {
	t.Helper()
	var out bytes.Buffer
	if code := Main([]string{args}, &out); code != 0 {
		t.Fatalf("Main code %d: %s", code, out.String())
	}
	result, err := decodeOutcome(out.Bytes())
	if err != nil {
		t.Fatalf("decode Main output: %v\n%s", err, out.String())
	}
	return result
}

func TestMainNestedGitignoreAndSnapshots(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, ".gitignore", "*.log\n")
	writeSearchFixture(t, root, "drop.log", "rare_token_zq\n")
	writeSearchFixture(t, root, "sub/.gitignore", "!keep.log\n")
	writeSearchFixture(t, root, "sub/keep.log", "rare_token_zq\n")
	writeSearchFixture(t, root, "src/a.go", "package a\n")
	result := runMain(t, `{"mode":"grep","pattern":"rare_token_zq"}`)
	if !strings.Contains(result.Text, "sub/keep.log:1: rare_token_zq") || strings.Contains(result.Text, "drop.log") {
		t.Fatalf("grep output %q", result.Text)
	}
	if len(result.Captures) != 1 || string(result.Captures[0].Data) != "rare_token_zq\n" {
		t.Fatalf("captures %+v", result.Captures)
	}
}

func TestMainFindAndGrepLimits(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "src/a.go", "package a\n")
	writeSearchFixture(t, root, "src/b.txt", "text\n")
	found := runMain(t, `{"mode":"find","pattern":"*.go"}`)
	if found.Text != "src/a.go" {
		t.Fatalf("find output %q", found.Text)
	}
	var body strings.Builder
	for i := 1; i <= 20; i++ {
		if i == 1 || i == 20 {
			body.WriteString("hit\n")
		} else {
			body.WriteString("line\n")
		}
	}
	writeSearchFixture(t, root, "hits.txt", body.String())
	grep := runMain(t, `{"mode":"grep","pattern":"hit","path":"hits.txt","limit":1}`)
	if !strings.Contains(grep.Text, "hits.txt:1: hit") || !strings.Contains(grep.Text, "hits.txt:20- hit") {
		t.Fatalf("context output %q", grep.Text)
	}
	if !strings.Contains(grep.Text, "[Truncated: 1 more matches]") {
		t.Fatalf("limit output %q", grep.Text)
	}
}

func TestMainLongLineCropAndLiteralCase(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "f.txt", "Needle"+strings.Repeat("é", 600)+"\n")
	result := runMain(t, `{"mode":"grep","pattern":"needle","ignore_case":true}`)
	first, _, _ := strings.Cut(result.Text, "\n")
	if !strings.HasSuffix(first, "...") || !strings.Contains(first, "Needle") {
		t.Fatalf("long line %q", first)
	}
	if len([]rune(first)) < 500 {
		t.Fatal("line was over-cropped")
	}
	if len(result.Captures[0].Shown) != 0 {
		t.Fatalf("cropped line was marked shown: %+v", result.Captures[0].Shown)
	}
}

func TestMainGitignoreNegatedClass(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, ".gitignore", "[!a].txt\n")
	writeSearchFixture(t, root, "a.txt", "class_token\n")
	writeSearchFixture(t, root, "b.txt", "class_token\n")
	result := runMain(t, `{"mode":"grep","pattern":"class_token"}`)
	if result.Text != "a.txt:1: class_token" {
		t.Fatalf("negated class output %q", result.Text)
	}
}

func TestMainRejectsUnsupportedRegexp(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "a.txt", "x\n")
	var out bytes.Buffer
	if code := Main([]string{`{"mode":"grep","pattern":"(?=x)"}`}, &out); code == 0 {
		t.Fatal("lookahead was accepted as RE2")
	}
}

func TestMainFindOmitsDirectories(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "src/a.txt", "x\n")
	result := runMain(t, `{"mode":"find","pattern":"*"}`)
	if result.Text != "src/a.txt" {
		t.Fatalf("find output %q", result.Text)
	}
}

type fakeProcessRunner struct {
	runner.Runner
	data []byte
	read bool
	ids  []string
}

func (r *fakeProcessRunner) Start(ctx context.Context, spec runner.ProcessSpec) error {
	r.ids = append(r.ids, spec.ID)
	r.read = false
	if len(spec.Argv) != 3 || spec.Argv[0] != "@self" || spec.Argv[1] != "__search" {
		return errors.New("unexpected process")
	}
	parsed, err := parseArgs(jsontext.Value(spec.Argv[2]))
	if err != nil {
		return err
	}
	result, err := execute(spec.Dir, parsed)
	if err != nil {
		return err
	}
	r.data = encodeOutcome(result)
	return nil
}

func (r *fakeProcessRunner) Read(ctx context.Context, id string, offset int64) (runner.Output, error) {
	if r.read {
		return runner.Output{Exit: &runner.Exit{Code: 0}}, nil
	}
	r.read = true
	return runner.Output{Data: r.data, Next: int64(len(r.data)), Exit: &runner.Exit{Code: 0}}, nil
}

func (r *fakeProcessRunner) Signal(ctx context.Context, id string, signal runner.Signal) error {
	return nil
}

func firstCaptureMeta(t *testing.T, meta jsontext.Value) jsontext.Value {
	t.Helper()
	var captures []jsontext.Value
	err := eachField(meta, func(key string, value jsontext.Value) error {
		if key != "captures" {
			return nil
		}
		return eachItem(value, func(item jsontext.Value) error {
			captures = append(captures, item)
			return nil
		})
	})
	if err != nil || len(captures) == 0 {
		t.Fatalf("captures metadata %s: %v", meta, err)
	}
	return captures[0]
}

func runSearchTool(t *testing.T, root, args string) tool.Result {
	t.Helper()
	local, err := runner.NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	env := tool.Env{Runner: &fakeProcessRunner{Runner: local}, Cwd: root, Snapshots: snapshot.New()}
	result, err := New().Run(t.Context(), tool.Call{ID: "search", Args: jsontext.Value(args)}, env)
	if err != nil {
		t.Fatal(err)
	}
	return result
}

func TestToolRunsSearchThroughRunner(t *testing.T) {
	root := t.TempDir()
	writeSearchFixture(t, root, "f.txt", "alpha\n")
	result := runSearchTool(t, root, `{"mode":"grep","pattern":"alpha"}`)
	meta := firstCaptureMeta(t, result.Meta)
	rebuilt := snapshot.New()
	if err := rebuilt.Restore(meta, []byte("alpha\n")); err != nil {
		t.Fatal(err)
	}
	_, rest, _ := strings.Cut(string(meta), `"ref":"`)
	ref, _, _ := strings.Cut(rest, `"`)
	entry, err := rebuilt.Get(ref)
	if err != nil || !rebuilt.Covers(ref, 1, 1) {
		t.Fatalf("restored search capture %+v: %v", entry, err)
	}
	expected := "f.txt:1: alpha\n[f.txt#" + snapshot.Tag("whole", entry.Data) + "]\n" +
		"[f.txt#" + snapshot.VersionTag(entry.Data) + "]\n[f.txt@" + ref + "]\n"
	if string(result.Output) != expected {
		t.Fatalf("tool output %q, want %q", result.Output, expected)
	}
	entry.Data[0] = 'X'
	if again, againErr := rebuilt.Get(ref); againErr != nil || string(again.Data) != "alpha\n" {
		t.Fatal("search snapshot buffer is mutable")
	}
}
