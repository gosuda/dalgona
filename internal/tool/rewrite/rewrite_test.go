package rewrite

import (
	"bytes"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/syntax"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func setup(t *testing.T, files map[string]string) tool.Env {
	t.Helper()
	cwd := t.TempDir()
	for name, body := range files {
		path := filepath.Join(cwd, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	local, err := runner.NewLocal(filepath.Join(t.TempDir(), "runs"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	return tool.Env{Runner: local, Cwd: cwd, Home: t.TempDir(), Snapshots: snapshot.New()}
}

func callArgs(t *testing.T, a args) jsontext.Value {
	t.Helper()
	raw, err := json.Marshal(a)
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func run(t *testing.T, env tool.Env, a args) (tool.Result, error) {
	t.Helper()
	return runWith(t, Editor{}, env, a)
}

func runWith(t *testing.T, editor Editor, env tool.Env, a args) (tool.Result, error) {
	t.Helper()
	return editor.Run(t.Context(), tool.Call{ID: "call-1", Name: "rewrite", Args: callArgs(t, a)}, env)
}

func readFile(t *testing.T, env tool.Env, name string) string {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(env.Cwd, filepath.FromSlash(name)))
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}

func workspaceBytes(t *testing.T, env tool.Env) map[string]string {
	t.Helper()
	files := map[string]string{}
	root := os.DirFS(env.Cwd)
	err := fs.WalkDir(root, ".", func(name string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil || entry.IsDir() {
			return walkErr
		}
		data, readErr := fs.ReadFile(root, name)
		files[name] = string(data)
		return readErr
	})
	if err != nil {
		t.Fatal(err)
	}
	return files
}

func wantErr(t *testing.T, env tool.Env, a args, want string) {
	t.Helper()
	before := workspaceBytes(t, env)
	plan, err := New().Plan(t.Context(), tool.Call{ID: "call-1", Name: "rewrite", Args: callArgs(t, a)}, env)
	if err == nil || err.Error() != want {
		t.Fatalf("error = %v, want %q", err, want)
	}
	if plan.Apply != nil || plan.Discard != nil || len(plan.Files) != 0 {
		t.Fatalf("a refused call returned a plan: %+v", plan)
	}
	if !reflect.DeepEqual(workspaceBytes(t, env), before) {
		t.Fatal("a refused call changed the workspace")
	}
}

func goCall(pattern, target, replace string) args {
	return args{Lang: "go", Pattern: pattern, Target: target, Replace: replace}
}

const funcName = "(function_declaration name: (identifier) @name)"

func TestRewriteRenamesCalledFunctionInTwoFiles(t *testing.T) {
	env := setup(t, map[string]string{
		"a/a.go": "package a\n\nfunc Old() int { return 1 }\n\nfunc Use() int { return Old() }\n",
		"b/b.go": "package b\n\nfunc Caller() int { return Old() }\n",
	})
	result, err := run(t, env, goCall(`((identifier) @name (#eq? @name "Old"))`, "name", "New"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(string(result.Output), "rewrite: 3 replacements in 2 files\na/a.go: 2 replacements\nb/b.go: 1 replacements\n") {
		t.Fatalf("output = %q", result.Output)
	}
	if got := readFile(t, env, "a/a.go"); got != "package a\n\nfunc New() int { return 1 }\n\nfunc Use() int { return New() }\n" {
		t.Fatalf("a.go = %q", got)
	}
	if got := readFile(t, env, "b/b.go"); got != "package b\n\nfunc Caller() int { return New() }\n" {
		t.Fatalf("b.go = %q", got)
	}
}

func TestRewriteExpandsCaptureAndLiteralAt(t *testing.T) {
	env := setup(t, map[string]string{"a.go": "package a\n\n// Hi\n"})
	if _, err := run(t, env, goCall("(comment) @name", "name", "// @name @@")); err != nil {
		t.Fatal(err)
	}
	if got := readFile(t, env, "a.go"); got != "package a\n\n// // Hi @\n" {
		t.Fatalf("a.go = %q", got)
	}
}

func TestRewriteReplacementThatStillMatchesRunsAgain(t *testing.T) {
	env := setup(t, map[string]string{"a.go": "package a\n\n// Hi\n"})
	a := goCall("(comment) @name", "name", "// @name")
	if _, err := run(t, env, a); err != nil {
		t.Fatal(err)
	}
	if got := readFile(t, env, "a.go"); got != "package a\n\n// // Hi\n" {
		t.Fatalf("first call a.go = %q", got)
	}
	if _, err := run(t, env, a); err != nil {
		t.Fatal(err)
	}
	if got := readFile(t, env, "a.go"); got != "package a\n\n// // // Hi\n" {
		t.Fatalf("second call a.go = %q", got)
	}
}

func TestRewriteSplicesBytesInCRLFFile(t *testing.T) {
	env := setup(t, map[string]string{"a.go": "package a\r\n\r\nvar s = 1\r\nvar t = 2\r\n"})
	if _, err := run(t, env, goCall("(int_literal) @name", "name", "3 /* x\n y */")); err != nil {
		t.Fatal(err)
	}
	if got := readFile(t, env, "a.go"); got != "package a\r\n\r\nvar s = 3 /* x\n y */\r\nvar t = 3 /* x\n y */\r\n" {
		t.Fatalf("a.go = %q", got)
	}
}

func TestRewriteRefusesBadQuery(t *testing.T) {
	env := setup(t, map[string]string{"a.go": "package a\n"})
	lang, ok := syntax.ByName("go")
	if !ok {
		t.Fatal("go grammar missing")
	}
	_, compileErr := lang.Compile("(function_declaration")
	if compileErr == nil {
		t.Fatal("query compiled")
	}
	wantErr(t, env, goCall("(function_declaration", "name", "x"), "rewrite: "+compileErr.Error())
}

func TestRewriteRefusesUnknownLang(t *testing.T) {
	env := setup(t, map[string]string{"a.go": "package a\n"})
	wantErr(t, env, args{Lang: "cobol", Pattern: funcName, Target: "name", Replace: "x"},
		"rewrite: unknown lang cobol, use one of: "+strings.Join(syntax.Names(), ", "))
}

func TestRewriteRefusesUnknownTarget(t *testing.T) {
	env := setup(t, map[string]string{"a.go": "package a\n"})
	wantErr(t, env, goCall(funcName, "missing", "x"), "rewrite: target missing is not a capture of the query")
}

func TestRewriteRefusesUnknownReplaceCapture(t *testing.T) {
	env := setup(t, map[string]string{"a.go": "package a\n"})
	wantErr(t, env, goCall(funcName, "name", "@other"), "rewrite: replace names @other, which the query does not capture")
}

func TestRewriteRefusesLoneAt(t *testing.T) {
	env := setup(t, map[string]string{"a.go": "package a\n"})
	wantErr(t, env, goCall(funcName, "name", "a @ b"), "rewrite: replace has a lone @ at byte 2; write @@ for a literal @")
}

func TestRewriteRefusesOverlappingTargets(t *testing.T) {
	env := setup(t, map[string]string{"a.go": "package a\n\nfunc F() {\n\tif true {\n\t}\n}\n"})
	wantErr(t, env, goCall("(block) @name", "name", "{}"), "rewrite: targets overlap in a.go at line 4")
}

func TestRewriteRefusesFileWithSyntaxErrorsBefore(t *testing.T) {
	const body = "package a\n\nfunc Keep( {\n"
	env := setup(t, map[string]string{"a.go": body})
	wantErr(t, env, goCall(funcName, "name", "Renamed"), "rewrite: a.go has syntax errors; fix it with patch first")
	if got := readFile(t, env, "a.go"); got != body {
		t.Fatalf("a.go = %q", got)
	}
}

func TestRewriteSyntaxBreakLeavesFilesUnchanged(t *testing.T) {
	const first = "package a\n\nfunc Keep() int { return 1 }\n"
	const second = "package b\n\nfunc Other() int { return 2 }\n"
	env := setup(t, map[string]string{"a.go": first, "b.go": second})
	wantErr(t, env, goCall(funcName, "name", "Renamed("), "rewrite: a.go has a syntax error at line 3 after the rewrite; nothing was written.")
	if readFile(t, env, "a.go") != first || readFile(t, env, "b.go") != second {
		t.Fatal("a refused call wrote a file")
	}
}

func TestRewriteStoppedParseIsRefused(t *testing.T) {
	err := checkStopped("a.go", syntax.Report{Stopped: true})
	if err == nil || err.Error() != "rewrite: parse stopped for a.go; nothing was written." {
		t.Fatalf("error = %v", err)
	}
}

func TestRewriteRefusesOversizeFile(t *testing.T) {
	body := "package a\n\nvar s = \"" + strings.Repeat("a", 1<<20) + "\"\n"
	env := setup(t, map[string]string{"a.go": body})
	wantErr(t, env, goCall(funcName, "name", "X"), "rewrite: a.go is over 1048576 bytes; narrow path or glob.")
}

func TestRewriteRefusesTooManyFiles(t *testing.T) {
	files := map[string]string{}
	for i := range 33 {
		files[fmt.Sprintf("pkg/f%02d.go", i)] = "package p\n\nfunc Target() {}\n"
	}
	env := setup(t, files)
	a := goCall(funcName, "name", "Other")
	a.Path = "pkg"
	wantErr(t, env, a, "rewrite: 33 files would change; the limit is 32. Narrow path or glob.")
	for name, body := range files {
		if readFile(t, env, name) != body {
			t.Fatalf("%s changed after the refusal", name)
		}
	}
}

func TestRewriteRefusesTooManyEdits(t *testing.T) {
	var text strings.Builder
	text.WriteString("package a\n\n")
	for i := range 257 {
		fmt.Fprintf(&text, "func F%d() {}\n", i)
	}
	content := text.String()
	env := setup(t, map[string]string{"a.go": content})
	wantErr(t, env, goCall(funcName, "name", "G"), "rewrite: 257 replacements would change; the limit is 256. Narrow path or glob.")
	if readFile(t, env, "a.go") != content {
		t.Fatal("a.go changed after the refusal")
	}
}

func TestRewriteRefusesTooManyCandidates(t *testing.T) {
	files := map[string]string{}
	for i := range 257 {
		files[fmt.Sprintf("pkg/f%03d.go", i)] = "package p\n"
	}
	env := setup(t, files)
	a := goCall("(package_clause (package_identifier) @name)", "name", "q")
	a.Path = "pkg"
	wantErr(t, env, a, "rewrite: 257 files match; the limit is 256. Narrow path or glob.")
}

func TestRewriteNoMatch(t *testing.T) {
	const body = "package a\n\nfunc Hi() {}\n"
	env := setup(t, map[string]string{"a.go": body})
	const pattern = `(function_declaration name: (identifier) @name (#eq? @name "Missing"))`
	result, err := run(t, env, goCall(pattern, "name", "X"))
	if err != nil {
		t.Fatal(err)
	}
	if string(result.Output) != "No matches for "+pattern {
		t.Fatalf("output = %q", result.Output)
	}
	if readFile(t, env, "a.go") != body {
		t.Fatal("a no-match call wrote a file")
	}
}

type nonLocal struct{ runner.Runner }

func TestRewriteRefusesNonLocalRunner(t *testing.T) {
	env := setup(t, map[string]string{"a.go": "package a\n"})
	env.Runner = nonLocal{env.Runner}
	wantErr(t, env, goCall(funcName, "name", "X"), "rewrite: the rewrite tool needs the local runner; this runner is not local")
}

func TestRewriteSchemaLangMatchesGrammars(t *testing.T) {
	var doc struct {
		Properties struct {
			Lang struct {
				Enum []string `json:"enum"`
			} `json:"lang"`
		} `json:"properties"`
	}
	if err := json.Unmarshal([]byte(New().Spec().Schema), &doc); err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(doc.Properties.Lang.Enum, syntax.Names()) {
		t.Fatalf("schema enum = %v, grammars = %v", doc.Properties.Lang.Enum, syntax.Names())
	}
}

func TestRewritePlanIsDeterministic(t *testing.T) {
	env := setup(t, map[string]string{
		"a.go": "package a\n\nfunc Old() {}\n",
		"b.go": "package b\n\nfunc Old() {}\n",
	})
	a := goCall(funcName, "name", "New")
	first, err := New().Plan(t.Context(), tool.Call{ID: "call-1", Name: "rewrite", Args: callArgs(t, a)}, env)
	if err != nil {
		t.Fatal(err)
	}
	defer first.Discard()
	second, err := New().Plan(t.Context(), tool.Call{ID: "call-2", Name: "rewrite", Args: callArgs(t, a)}, env)
	if err != nil {
		t.Fatal(err)
	}
	defer second.Discard()
	if len(first.Files) != 2 || len(second.Files) != len(first.Files) {
		t.Fatalf("files = %d and %d, want 2", len(first.Files), len(second.Files))
	}
	sameFiles(t, first.Files, second.Files)
}

func sameFiles(t *testing.T, first, second []tool.FileChange) {
	t.Helper()
	for i := range first {
		x, y := first[i], second[i]
		if x.Path != y.Path || x.PreSHA != y.PreSHA || x.PostSHA != y.PostSHA || !bytes.Equal(x.Pre, y.Pre) {
			t.Fatalf("file %d differs: %+v and %+v", i, x, y)
		}
	}
}

func TestRewriteStaleFileRefusedAtApply(t *testing.T) {
	env := setup(t, map[string]string{"a.go": "package a\n\nfunc Old() {}\n"})
	plan, err := New().Plan(t.Context(), tool.Call{ID: "call-1", Name: "rewrite", Args: callArgs(t, goCall(funcName, "name", "New"))}, env)
	if err != nil {
		t.Fatal(err)
	}
	const outside = "package a\n\nfunc Other() {}\n"
	if writeErr := os.WriteFile(filepath.Join(env.Cwd, "a.go"), []byte(outside), 0o600); writeErr != nil {
		t.Fatal(writeErr)
	}
	_, err = plan.Apply(t.Context())
	if err == nil || err.Error() != "patch: a.go changed while you were approving. Nothing was written. Try again" {
		t.Fatalf("apply error = %v", err)
	}
	if readFile(t, env, "a.go") != outside {
		t.Fatal("a stale apply overwrote the file")
	}
}

func TestRewriteRefusesSelectedFileOverSearchCap(t *testing.T) {
	const body = "package a\n\nfunc Target() {}\n"
	env := setup(t, map[string]string{"a.go": body})
	big := filepath.Join(env.Cwd, "big.go")
	if err := os.WriteFile(big, []byte("package big\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Truncate(big, 16<<20+1); err != nil {
		t.Fatal(err)
	}
	a := goCall(funcName, "name", "Other")
	wantErr(t, env, a, "rewrite: big.go is over 1048576 bytes; narrow path or glob.")
	if readFile(t, env, "a.go") != body {
		t.Fatal("a.go changed after the refusal")
	}
	a.Glob = "a.go"
	if _, err := run(t, env, a); err != nil {
		t.Fatal(err)
	}
	if want := "package a\n\nfunc Other() {}\n"; readFile(t, env, "a.go") != want {
		t.Fatalf("a.go = %q, want %q", readFile(t, env, "a.go"), want)
	}
}

func TestRewriteSeesSelectedFileWithNULBytes(t *testing.T) {
	const body = "package a\n\nfunc Target() {}\n"
	env := setup(t, map[string]string{
		"a.go":   body,
		"bad.go": "package bad\n\nfunc Keep( {\n\x00",
	})
	a := goCall(funcName, "name", "Other")
	wantErr(t, env, a, "rewrite: bad.go has syntax errors; fix it with patch first")
	if readFile(t, env, "a.go") != body {
		t.Fatal("a.go changed after the refusal")
	}
	a.Glob = "a.go"
	if _, err := run(t, env, a); err != nil {
		t.Fatal(err)
	}
	if want := "package a\n\nfunc Other() {}\n"; readFile(t, env, "a.go") != want {
		t.Fatalf("a.go = %q, want %q", readFile(t, env, "a.go"), want)
	}
}

func TestRewriteAppliesThirtyTwoFiles(t *testing.T) {
	files := map[string]string{}
	for i := range 32 {
		files[fmt.Sprintf("pkg/f%02d.go", i)] = "package p\n\nfunc Target() {}\n"
	}
	env := setup(t, files)
	a := goCall(funcName, "name", "Other")
	a.Path = "pkg"
	if _, err := run(t, env, a); err != nil {
		t.Fatal(err)
	}
	for name := range files {
		if got := readFile(t, env, name); !strings.Contains(got, "func Other()") {
			t.Fatalf("%s = %q, want the rename", name, got)
		}
	}
}

func TestRewriteApplies256Edits(t *testing.T) {
	var text strings.Builder
	text.WriteString("package a\n\n")
	for i := range 256 {
		fmt.Fprintf(&text, "func F%d() {}\n", i)
	}
	env := setup(t, map[string]string{"a.go": text.String()})
	if _, err := run(t, env, goCall(funcName, "name", "G")); err != nil {
		t.Fatal(err)
	}
	got := readFile(t, env, "a.go")
	if strings.Count(got, "func G") != 256 || strings.Contains(got, "func F") {
		t.Fatalf("a.go holds %d renamed funcs and keeps F names: %v", strings.Count(got, "func G"), strings.Contains(got, "func F"))
	}
}

func TestRewriteRefusesFileThatGrowsPastLimitDuringRead(t *testing.T) {
	const body = "package a\n\nfunc Target() {}\n"
	env := setup(t, map[string]string{"a.go": body})
	grown := body + strings.Repeat("a", rewriteMaxFileBytes)
	fired := false
	editor := Editor{beforeRead: func(path string) {
		if fired || filepath.Base(path) != "a.go" {
			return
		}
		fired = true
		if err := os.WriteFile(path, []byte(grown), 0o600); err != nil {
			t.Error(err)
		}
	}}
	_, err := runWith(t, editor, env, goCall(funcName, "name", "Other"))
	if want := "rewrite: a.go is over 1048576 bytes; narrow path or glob."; err == nil || err.Error() != want {
		t.Fatalf("error = %v, want %q", err, want)
	}
	if !fired {
		t.Fatal("the read hook did not run")
	}
	if readFile(t, env, "a.go") != grown {
		t.Fatal("the rewrite wrote over the grown file")
	}
}

func TestRewriteAppliesFileThatGrowsWithinLimitDuringRead(t *testing.T) {
	const body = "package a\n\nfunc Target() {}\n"
	const grown = "package a\n\nfunc Target() {}\n\nvar Added = 1\n"
	env := setup(t, map[string]string{"a.go": body})
	fired := false
	editor := Editor{beforeRead: func(path string) {
		if fired {
			return
		}
		fired = true
		if err := os.WriteFile(path, []byte(grown), 0o600); err != nil {
			t.Error(err)
		}
	}}
	if _, err := runWith(t, editor, env, goCall(funcName, "name", "Other")); err != nil {
		t.Fatal(err)
	}
	if !fired {
		t.Fatal("the read hook did not run")
	}
	if want := "package a\n\nfunc Other() {}\n\nvar Added = 1\n"; readFile(t, env, "a.go") != want {
		t.Fatalf("a.go = %q, want %q", readFile(t, env, "a.go"), want)
	}
}

func TestCollectionReleasesImagesPastLimits(t *testing.T) {
	cases := []struct {
		name         string
		files        int
		editsEach    int
		wantRetained int
	}{
		{"32 files", rewriteMaxFiles, 1, rewriteMaxFiles},
		{"33 files", rewriteMaxFiles + 1, 1, 0},
		{"256 edits", 1, rewriteMaxEdits, 1},
		{"257 edits", 1, rewriteMaxEdits + 1, 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var got collection
			for range tc.files {
				got.add(changed{path: "p.go", src: []byte("s"), post: []byte("p"), edits: make([]edit, tc.editsEach)})
			}
			if got.files != tc.files || got.edits != tc.files*tc.editsEach {
				t.Fatalf("counted %d files and %d edits, want %d and %d", got.files, got.edits, tc.files, tc.files*tc.editsEach)
			}
			if len(got.retained) != tc.wantRetained {
				t.Fatalf("retained %d files, want %d", len(got.retained), tc.wantRetained)
			}
		})
	}
}

func TestCollectionKeepsCountingAfterRelease(t *testing.T) {
	var got collection
	for range rewriteMaxFiles + 3 {
		got.add(changed{path: "p.go", src: []byte("s"), post: []byte("p"), edits: make([]edit, 2)})
	}
	if got.files != rewriteMaxFiles+3 || got.edits != 2*(rewriteMaxFiles+3) || len(got.retained) != 0 {
		t.Fatalf("files = %d, edits = %d, retained = %d", got.files, got.edits, len(got.retained))
	}
}
