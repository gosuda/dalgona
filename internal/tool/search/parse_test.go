package search

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func runMainError(t *testing.T, args string) string {
	t.Helper()
	var out bytes.Buffer
	if code := Main([]string{args}, &out); code != 2 {
		t.Fatalf("Main code %d, want 2: %s", code, out.String())
	}
	return out.String()
}

func TestSymbolsOutlineOfTree(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "src/a.go", "package a\n\ntype T struct{}\n\nfunc (T) M() {}\n")
	got := runMain(t, `{"mode":"symbols","path":"src"}`).Text
	want := "src/a.go:3-3: type type T struct{}\nsrc/a.go:5-5: method func (T) M() {}"
	if got != want {
		t.Fatalf("symbols = %q, want %q", got, want)
	}
}

func TestSymbolsNestingTypeScriptClassMethod(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "src/c.ts", "export class C {\n  m(): void {}\n}\n")
	got := runMain(t, `{"mode":"symbols","path":"src"}`).Text
	want := "src/c.ts:1-3: class export class C {\n  src/c.ts:2-2: method m(): void {}"
	if got != want {
		t.Fatalf("symbols = %q, want %q", got, want)
	}
}

func TestSymbolsKindFilter(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "src/c.ts", "export class C {\n  m(): void {}\n}\n")
	got := runMain(t, `{"mode":"symbols","path":"src","kind":["method"]}`).Text
	if got != "  src/c.ts:2-2: method m(): void {}" {
		t.Fatalf("symbols = %q", got)
	}
}

func TestSymbolsNamePatterns(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "a.go", "package a\n\nfunc Run() {}\n\nfunc run_all() {}\n\nfunc Runner() {}\n")
	cases := []struct {
		name, args, want string
	}{
		{"case sensitive", `{"mode":"symbols","pattern":"run"}`, "a.go:5-5: function func run_all() {}"},
		{"ignore case", `{"mode":"symbols","pattern":"run","ignore_case":true}`, "a.go:3-3: function func Run() {}\na.go:5-5: function func run_all() {}\na.go:7-7: function func Runner() {}"},
		{"regexp", `{"mode":"symbols","pattern":"R.n"}`, "a.go:3-3: function func Run() {}\na.go:7-7: function func Runner() {}"},
		{"literal dot", `{"mode":"symbols","pattern":"R.n","literal":true}`, "No matches for R.n. Searched 1 files of supported languages."},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := runMain(t, tc.args).Text; got != tc.want {
				t.Fatalf("symbols = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestSymbolsRefusesAnchors(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "a.go", "package a\n")
	got := runMainError(t, `{"mode":"symbols","pattern":"^Run$"}`)
	want := `search: name patterns cannot use ^, $, \A, \z, \b or \B; match the name only`
	if got != want {
		t.Fatalf("error = %q, want %q", got, want)
	}
}

func TestSymbolsNotParsedFooter(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	for i := range 300 {
		writeSearchFixture(t, root, fmt.Sprintf("pkg/f%03d.go", i), "package p\n\nfunc Target() {}\n")
	}
	lines := strings.Split(runMain(t, `{"mode":"symbols","path":"pkg","limit":1000}`).Text, "\n")
	if len(lines) != 257 {
		t.Fatalf("got %d lines, want 256 rows and one footer", len(lines))
	}
	if lines[0] != "pkg/f000.go:3-3: function func Target() {}" {
		t.Fatalf("first row %q", lines[0])
	}
	if lines[256] != "[Not parsed: 44 files. Narrow path or glob.]" {
		t.Fatalf("footer %q", lines[256])
	}
}

func TestSymbolsLimitFooter(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "a.go", "package a\n\nfunc F1() {}\nfunc F2() {}\nfunc F3() {}\nfunc F4() {}\nfunc F5() {}\n")
	got := runMain(t, `{"mode":"symbols","limit":2}`).Text
	want := "a.go:3-3: function func F1() {}\na.go:4-4: function func F2() {}\n[Truncated: 3 more symbols]"
	if got != want {
		t.Fatalf("symbols = %q, want %q", got, want)
	}
}

func TestSymbolsNoMatch(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "a.go", "package a\n\nfunc A() {}\n")
	got := runMain(t, `{"mode":"symbols","pattern":"Missing"}`).Text
	if got != "No matches for Missing. Searched 1 files of supported languages." {
		t.Fatalf("symbols = %q", got)
	}
}

func TestSymbolsSyntaxErrorsKeepRows(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "src/broken.go", "package b\n\nfunc Good() {}\n\nfunc Bad( {\n")
	got := runMain(t, `{"mode":"symbols","path":"src"}`).Text
	if !strings.Contains(got, "src/broken.go:3-3: function func Good() {}") {
		t.Fatalf("rows missing from %q", got)
	}
	if !strings.HasSuffix(got, "[Parse: syntax errors, rows kept: src/broken.go]") {
		t.Fatalf("footer missing from %q", got)
	}
}

func TestSymbolsDeterminism(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	for i := range 50 {
		writeSearchFixture(t, root, fmt.Sprintf("d/f%02d.go", i), fmt.Sprintf("package p\n\nfunc F%02d() {}\n", i))
	}
	var first, second bytes.Buffer
	if code := Main([]string{`{"mode":"symbols","path":"d"}`}, &first); code != 0 {
		t.Fatalf("first call code %d", code)
	}
	if code := Main([]string{`{"mode":"symbols","path":"d"}`}, &second); code != 0 {
		t.Fatalf("second call code %d", code)
	}
	if !bytes.Equal(first.Bytes(), second.Bytes()) {
		t.Fatal("two calls differ")
	}
}

func TestSymbolsInvalidUTF8AndNULSkipped(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "bad.go", "package p\n\nfunc B() {}\n\xff\n")
	writeSearchFixture(t, root, "nul.go", "package p\x00\n\nfunc N() {}\n")
	writeSearchFixture(t, root, "good.go", "package p\n\nfunc G() {}\n")
	got := runMain(t, `{"mode":"symbols"}`).Text
	if got != "good.go:3-3: function func G() {}" {
		t.Fatalf("symbols = %q", got)
	}
}

func TestSymbolsBudgetStopsNewFiles(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	for _, name := range []string{"a.go", "b.go", "c.go"} {
		writeSearchFixture(t, root, name, "package p\n\nfunc "+strings.TrimSuffix(name, ".go")+"() {}\n")
	}
	parsed := args{Mode: "symbols", Path: ".", Limit: 200}
	entries, err := walkScope(root, root)
	if err != nil {
		t.Fatal(err)
	}
	start := time.Unix(1_700_000_000, 0)
	calls := 0
	now := func() time.Time {
		calls++
		if calls == 1 {
			return start
		}
		return start.Add(11 * time.Second)
	}
	out, err := symbolsMode(context.Background(), entries, parsed, start, now)
	if err != nil {
		t.Fatal(err)
	}
	want := "a.go:3-3: function func a() {}\n[Stopped: 2 files not parsed. Narrow path or glob.]"
	if out.Text != want {
		t.Fatalf("symbols = %q, want %q", out.Text, want)
	}
}

func TestParseFootersRender(t *testing.T) {
	tally := callTally{
		parseNotes: []string{"[Parse: stopped, no rows: a.go]", "[Parse: syntax errors, rows kept: b.go]"},
		notParsed:  3,
		syntaxOnly: 2,
		stopped:    4,
	}
	want := []string{
		"[Parse: stopped, no rows: a.go]",
		"[Parse: syntax errors, rows kept: b.go]",
		"[Not parsed: 3 files. Narrow path or glob.]",
		"[Syntax only: 2 files. Use mode syntax with lang.]",
		"[Stopped: 4 files not parsed. Narrow path or glob.]",
	}
	if got := tally.footers(); !slices.Equal(got, want) {
		t.Fatalf("footers = %q, want %q", got, want)
	}
}

func TestSymbolsUnknownKind(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	got := runMainError(t, `{"mode":"symbols","kind":["record"]}`)
	if got != "search: unknown kind record" {
		t.Fatalf("error = %q", got)
	}
}

func TestKindOnlyForSymbols(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	got := runMainError(t, `{"mode":"find","pattern":"*.go","kind":["function"]}`)
	if got != "search: kind applies only to symbols and definition" {
		t.Fatalf("error = %q", got)
	}
}

func TestFindRequiresPattern(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	got := runMainError(t, `{"mode":"find"}`)
	if got != "search: pattern must not be empty" {
		t.Fatalf("error = %q", got)
	}
}

func TestDefinitionOneDefinition(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "src/a.go", "package a\n\nfunc Alpha() int {\n\treturn 1\n}\n")
	got := runMain(t, `{"mode":"definition","pattern":"Alpha","path":"src"}`)
	want := "src/a.go:3-5: function func Alpha() int {\nsrc/a.go:3: func Alpha() int {\nsrc/a.go:4: \treturn 1\nsrc/a.go:5: }"
	if got.Text != want {
		t.Fatalf("definition = %q, want %q", got.Text, want)
	}
	shown := []snapshot.Range{{Start: 3, End: 5}}
	if len(got.Captures) != 1 || got.Captures[0].Path != "src/a.go" || !slices.Equal(got.Captures[0].Shown, shown) {
		t.Fatalf("captures = %+v, want one capture of src/a.go shown 3-5", got.Captures)
	}
}

func TestDefinitionSameNameTwoFiles(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "a/x.go", "package p\n\nfunc Shared() {}\n")
	writeSearchFixture(t, root, "b/x.go", "package p\n\nfunc Shared() {}\n")
	got := runMain(t, `{"mode":"definition","pattern":"Shared"}`).Text
	want := "a/x.go:3-3: function func Shared() {}\na/x.go:3: func Shared() {}\nb/x.go:3-3: function func Shared() {}\nb/x.go:3: func Shared() {}"
	if got != want {
		t.Fatalf("definition = %q, want %q", got, want)
	}
}

func TestDefinitionBodyCutByLimit(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	var source strings.Builder
	source.WriteString("package p\n\nfunc Long() {\n")
	for line := 4; line <= 32; line++ {
		fmt.Fprintf(&source, "\tv%d := %d\n", line, line)
	}
	source.WriteString("}\n")
	writeSearchFixture(t, root, "long.go", source.String())
	got := runMain(t, `{"mode":"definition","pattern":"Long","limit":10}`).Text
	var want strings.Builder
	want.WriteString("long.go:3-33: function func Long() {\n")
	want.WriteString("long.go:3: func Long() {\n")
	for line := 4; line <= 12; line++ {
		fmt.Fprintf(&want, "long.go:%d: \tv%d := %d\n", line, line, line)
	}
	want.WriteString("[Truncated: 21 more lines]")
	if got != want.String() {
		t.Fatalf("definition = %q, want %q", got, want.String())
	}
}

func TestDefinitionNoMatch(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "a.go", "package a\n\nfunc A() {}\n")
	got := runMain(t, `{"mode":"definition","pattern":"Missing"}`).Text
	if want := "No matches for Missing. Searched 1 files of supported languages."; got != want {
		t.Fatalf("definition = %q, want %q", got, want)
	}
}

func TestDefinitionRequiresPattern(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	got := runMainError(t, `{"mode":"definition"}`)
	if got != "search: pattern must not be empty" {
		t.Fatalf("error = %q", got)
	}
}

func TestDefinitionKindFilter(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "src/c.ts", "export class C {\n  m(): void {}\n}\n")
	got := runMain(t, `{"mode":"definition","pattern":"m","kind":["method"],"path":"src"}`).Text
	want := "  src/c.ts:2-2: method m(): void {}\nsrc/c.ts:2:   m(): void {}"
	if got != want {
		t.Fatalf("definition = %q, want %q", got, want)
	}
}

func TestKindListMatchesSchema(t *testing.T) {
	var schema struct {
		Properties struct {
			Kind struct {
				Items struct {
					Enum []string `json:"enum"`
				} `json:"items"`
			} `json:"kind"`
		} `json:"properties"`
	}
	if err := json.Unmarshal([]byte(Tool{}.Spec().Schema), &schema); err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(schema.Properties.Kind.Items.Enum, symbolKinds) {
		t.Fatalf("schema kinds %q, symbolKinds %q", schema.Properties.Kind.Items.Enum, symbolKinds)
	}
}

type hangingRunner struct {
	runner.Runner
	killed atomic.Int32
}

func (r *hangingRunner) Start(context.Context, runner.ProcessSpec) error { return nil }

func (r *hangingRunner) Read(context.Context, string, int64) (runner.Output, error) {
	if r.killed.Load() > 0 {
		return runner.Output{Exit: &runner.Exit{Code: 137}}, nil
	}
	return runner.Output{Alive: true}, nil
}

func (r *hangingRunner) Signal(context.Context, string, runner.Signal) error {
	r.killed.Add(1)
	return nil
}

func TestSymbolsDeadlineEndsChild(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		root := t.TempDir()
		hang := &hangingRunner{}
		env := tool.Env{Runner: hang, Cwd: root, Snapshots: snapshot.New()}
		call := tool.Call{ID: "symbols-deadline", Args: jsontext.Value(`{"mode":"symbols"}`)}
		_, err := Tool{}.Run(t.Context(), call, env)
		if err == nil || !strings.Contains(err.Error(), "search: parse deadline reached") {
			t.Fatalf("error = %v, want parse deadline", err)
		}
		if n := hang.killed.Load(); n != 1 {
			t.Fatalf("kill signals = %d, want 1", n)
		}
	})
}

type mainRunner struct {
	runner.Runner
	code int
	out  []byte
	read bool
}

func (r *mainRunner) Start(_ context.Context, spec runner.ProcessSpec) error {
	var buf bytes.Buffer
	r.code = Main(spec.Argv[2:], &buf)
	r.out = buf.Bytes()
	r.read = false
	return nil
}

func (r *mainRunner) Read(context.Context, string, int64) (runner.Output, error) {
	if r.read {
		return runner.Output{Exit: &runner.Exit{Code: r.code}}, nil
	}
	r.read = true
	return runner.Output{Data: r.out, Next: int64(len(r.out)), Exit: &runner.Exit{Code: r.code}}, nil
}

func (r *mainRunner) Signal(context.Context, string, runner.Signal) error { return nil }

func runToolError(t *testing.T, args string) error {
	t.Helper()
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "a.go", "package a\n\nfunc Run() {}\n")
	env := tool.Env{Runner: &mainRunner{}, Cwd: root, Snapshots: snapshot.New()}
	_, err := Tool{}.Run(t.Context(), tool.Call{ID: "refusal", Args: jsontext.Value(args)}, env)
	return err
}

func TestToolRunReturnsChildRefusalText(t *testing.T) {
	err := runToolError(t, `{"mode":"symbols","pattern":"^Run$"}`)
	if err == nil || err.Error() != anchorRefusal {
		t.Fatalf("error = %v, want %q", err, anchorRefusal)
	}
}

type refusingKillRunner struct{ runner.Runner }

func (r *refusingKillRunner) Start(context.Context, runner.ProcessSpec) error { return nil }

func (r *refusingKillRunner) Read(context.Context, string, int64) (runner.Output, error) {
	return runner.Output{Alive: true}, nil
}

func (r *refusingKillRunner) Signal(context.Context, string, runner.Signal) error {
	return errors.New("kill refused")
}

func TestSymbolsDeadlineKeepsKillError(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		env := tool.Env{Runner: &refusingKillRunner{}, Cwd: t.TempDir(), Snapshots: snapshot.New()}
		call := tool.Call{ID: "symbols-kill", Args: jsontext.Value(`{"mode":"symbols"}`)}
		_, err := Tool{}.Run(t.Context(), call, env)
		if err == nil || !strings.Contains(err.Error(), "search: parse deadline reached") || !strings.Contains(err.Error(), "kill refused") {
			t.Fatalf("error = %v, want the parse deadline and the kill error", err)
		}
	})
}

type lateExitRunner struct {
	runner.Runner
	killed bool
	reads  int
	exited bool
}

func (r *lateExitRunner) Start(context.Context, runner.ProcessSpec) error { return nil }

func (r *lateExitRunner) Read(context.Context, string, int64) (runner.Output, error) {
	if !r.killed {
		return runner.Output{Alive: true}, nil
	}
	r.reads++
	if r.reads < 3 {
		return runner.Output{Alive: true}, nil
	}
	r.exited = true
	return runner.Output{Exit: &runner.Exit{Code: 137}}, nil
}

func (r *lateExitRunner) Signal(context.Context, string, runner.Signal) error {
	r.killed = true
	return nil
}

func TestSymbolsDeadlineWaitsForExit(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		late := &lateExitRunner{}
		env := tool.Env{Runner: late, Cwd: t.TempDir(), Snapshots: snapshot.New()}
		call := tool.Call{ID: "symbols-late", Args: jsontext.Value(`{"mode":"symbols"}`)}
		_, err := Tool{}.Run(t.Context(), call, env)
		if err == nil || !strings.Contains(err.Error(), "search: parse deadline reached") {
			t.Fatalf("error = %v, want parse deadline", err)
		}
		if !late.exited {
			t.Fatal("Run returned before the child exited")
		}
	})
}

func TestDefinitionRefusesAnchorExactText(t *testing.T) {
	err := runToolError(t, `{"mode":"definition","pattern":"^Run$"}`)
	if err == nil || err.Error() != anchorRefusal {
		t.Fatalf("error = %v, want %q", err, anchorRefusal)
	}
}
