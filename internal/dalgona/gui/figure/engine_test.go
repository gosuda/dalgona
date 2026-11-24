package figure

import (
	"context"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"oss.terrastruct.com/d2/d2compiler"
)

func renderWith(t *testing.T, e *engines, kind Kind, source string, theme Theme) (*List, error) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	return e.render(ctx, figureRequest{kind: kind, source: source, theme: theme, scale: 1})
}

func requireLabels(t *testing.T, l *List, want ...string) {
	t.Helper()
	labels := strings.Join(l.Labels, "|")
	for _, w := range want {
		if !strings.Contains(labels, w) {
			t.Errorf("labels %q lack %q", labels, w)
		}
	}
}

func requireImportRefused(t *testing.T, source string) {
	t.Helper()
	_, err := renderD2(context.Background(), d2Request{source: source, theme: testTheme})
	f := FailureOf(err)
	if err == nil || f.Code != FailureForbidden || !strings.Contains(f.Message, "imports are refused") || !f.Span.Known() {
		t.Fatalf("%q: error = %v, want a located import refusal", source, err)
	}
}

func TestD2RefusesLocalFileImport(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "other.d2"), []byte("secret: from disk"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Chdir(dir)
	for _, source := range []string{"x: @other\n", "a -> b\n...@other\n", "x: @other.d2\n"} {
		requireImportRefused(t, source)
	}
	_, err := renderD2(context.Background(), d2Request{source: "a -> b\nx: @other\n", theme: testTheme})
	if f := FailureOf(err); f.Span.Line != 2 || f.Span.Column != 4 {
		t.Fatalf("import span = %+v, want line 2 column 4", f.Span)
	}
	_, _, err = d2compiler.Compile("figure.d2", strings.NewReader("x: @other\n"), &d2compiler.CompileOptions{FS: refusedFS{}})
	if err == nil || !strings.Contains(err.Error(), fs.ErrPermission.Error()) {
		t.Fatalf("compiler with the refusing filesystem = %v, want a permission refusal", err)
	}
}

func TestD2RefusesResourcesAndPlugins(t *testing.T) {
	cases := map[string]string{
		"a.icon: https://example.com/x.svg":          "icons",
		"a: {shape: image; icon: ./x.png}":           "refused",
		"a.shape: image":                             "image shapes",
		"a: |latex\n\\notacommand{x}\n|":             "LaTeX",
		"vars: {d2-config: {layout-engine: elk}}\na": "layout engine",
		"vars: {d2-config: {sketch: true}}\na":       "sketch",
		"a.style.fill-pattern: dots":                 "fill patterns",
	}
	for source, word := range cases {
		_, err := renderD2(context.Background(), d2Request{source: source, theme: testTheme, formulas: testFonts().formulas()})
		if f := FailureOf(err); err == nil || !strings.Contains(f.Message, word) || !f.Span.Known() {
			t.Errorf("%q: error = %v, want a located refusal naming %q", source, err, word)
		}
	}
}

func TestD2RendersGrammarWithinBounds(t *testing.T) {
	e := &engines{}
	l, err := renderWith(t, e, KindD2, "direction: right\ngroup: 그룹 {\n  a: 시작 Start {shape: cylinder}\n  b: {shape: diamond; style.stroke-dash: 3}\n}\ngroup.a -> group.b: 연결 {style.stroke: red}\nc: {shape: person}\ngroup.b <-> c\nt: {shape: sql_table; id: int}\n", testTheme)
	if err != nil {
		t.Fatal(err)
	}
	requireLabels(t, l, "그룹", "시작 Start", "연결", "id", "int")
	if l.Width <= 0 || l.Height <= 0 || len(opsOf(l, OpFill)) < 6 {
		t.Fatalf("figure %gx%g with %d fills is not a drawn diagram", l.Width, l.Height, len(opsOf(l, OpFill)))
	}
	var edges strings.Builder
	for i := range GraphNodeLimit + 1 {
		edges.WriteString("n" + itoa(i) + "\n")
	}
	if _, err := renderWith(t, e, KindD2, edges.String(), testTheme); codeOf(err) != FailureLimit {
		t.Fatalf("1001 nodes = %v, want a limit failure", err)
	}
}

func TestEngineFilesMatchPinnedDigests(t *testing.T) {
	if err := verifyEngines(); err != nil {
		t.Fatal(err)
	}
	listed := string(engineSums)
	err := fs.WalkDir(engineFiles, "js", func(path string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() || path == "js/SHA256SUMS" || path == "js/NOTICE" {
			return err
		}
		if !strings.Contains(listed, "  "+strings.TrimPrefix(path, "js/")+"\n") {
			t.Errorf("vendored file %s has no pinned digest", path)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}

func TestScriptRuntimeExposesOnlyVendoredModules(t *testing.T) {
	for _, name := range []string{"fs", "node:fs", "std", "os", "https://example.com/x.js"} {
		if _, err := normalizeModule(nil, "beautiful-mermaid/index.js", name); err == nil {
			t.Errorf("module %q resolved", name)
		}
	}
	for _, name := range []string{"../../etc/passwd.js", "SHA256SUMS", "entities/missing.js"} {
		if _, err := loadModule(nil, name); err == nil {
			t.Errorf("module file %q loaded", name)
		}
	}
	vm, err := newNomnomlVM()
	if err != nil {
		t.Fatal(err)
	}
	defer vm.Close()
	out, err := vm.Eval(`[typeof process, typeof std, typeof os, typeof fetch, typeof XMLHttpRequest, typeof document, typeof window].join(",")`, 0)
	if err != nil {
		t.Fatal(err)
	}
	if out != strings.TrimSuffix(strings.Repeat("undefined,", 7), ",") {
		t.Fatalf("host globals = %v, want every host API undefined", out)
	}
}

func TestNomnomlRendersWithMeasuredLabels(t *testing.T) {
	e := &engines{}
	defer e.close()
	l, err := renderWith(t, e, KindNomnoml, "#direction: right\n[<frame>Frame|[A]->[B]]\n[Shop|name: string|buy()]-:>[<abstract>가나다라마바사]\n[<database>DB]<->[<actor>User]\n#.box: fill=#ff0000\n[<box>Styled]", testTheme)
	if err != nil {
		t.Fatal(err)
	}
	requireLabels(t, l, "Frame", "name: string", "buy()", "가나다라마바사", "Styled")
	_, err = renderWith(t, e, KindNomnoml, "[A]->[B]\n#import: secrets.nomnoml", testTheme)
	if f := FailureOf(err); f == nil || f.Code != FailureForbidden || f.Span.Line != 2 {
		t.Fatalf("nomnoml import = %v, want a forbidden failure on line 2", err)
	}
	_, err = renderWith(t, e, KindNomnoml, "[A]->[B\n", testTheme)
	if f := FailureOf(err); f == nil || f.Code != FailureInvalid {
		t.Fatalf("nomnoml syntax error = %v, want invalid input", err)
	}
}

func TestAdvanceTableMeasuresHangulAndSpaces(t *testing.T) {
	table, err := advanceTable("가i ")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(table, `"가":[`) || strings.Contains(table, `" ":[0,`) {
		t.Fatalf("advance table %s lacks Hangul or measures spaces as zero", table)
	}
}

var mermaidEngines = sync.OnceValues(func() (*engines, error) {
	e := &engines{}
	_, err := e.script(KindMermaid)
	return e, err
})

func loadedMermaid(t *testing.T) *engines {
	t.Helper()
	e, err := mermaidEngines()
	if err != nil {
		t.Fatalf("Mermaid engine did not load: %v", err)
	}
	return e
}

func TestMermaidFamiliesRenderAsData(t *testing.T) {
	e := loadedMermaid(t)
	families := map[string][]string{
		"graph TD\n  subgraph S [Group 그룹]\n  A[Start 시작] --> B{Is it?}\n  end\n  B -->|Yes| C[OK]\n  B -.->|No| D[\"\\\"); throw new Error(1); (\\\"\"]": {"Group 그룹", "Start 시작", "Yes", "No", "throw new Error"},
		"stateDiagram-v2\n  [*] --> Still\n  Still --> Moving":                                                 {"Still", "Moving"},
		"sequenceDiagram\n  Alice->>Bob: Hello 안녕\n  Bob-->>Alice: Hi":                                         {"Alice", "Bob", "Hello 안녕"},
		"classDiagram\n  class Animal {\n +eat() void\n }\n Animal <|-- Dog":                                   {"Animal", "Dog"},
		"erDiagram\n  CUSTOMER ||--o{ ORDER : places":                                                          {"CUSTOMER", "ORDER", "places"},
		"xychart-beta\n  title \"Sales\"\n  x-axis [jan, feb]\n  y-axis \"Revenue\" 0 --> 100\n  bar [10, 50]": {"Sales", "jan", "Revenue"},
	}
	for source, want := range families {
		t.Run(strings.SplitN(source, "\n", 2)[0], func(t *testing.T) {
			l, err := renderWith(t, e, KindMermaid, source, testTheme)
			if err != nil {
				t.Fatal(err)
			}
			requireLabels(t, l, want...)
		})
	}
	t.Run("theme change", func(t *testing.T) {
		dark := testTheme
		dark.Surface = Color{0x18, 0x19, 0x1b, 0xff}
		light, _ := renderWith(t, e, KindMermaid, "graph LR\nA-->B", testTheme)
		night, _ := renderWith(t, e, KindMermaid, "graph LR\nA-->B", dark)
		if light == nil || night == nil || opsOf(light, OpFill)[0].Paint.Color == opsOf(night, OpFill)[0].Paint.Color {
			t.Fatal("theme change did not change the figure background")
		}
	})
}
