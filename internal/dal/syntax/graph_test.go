package syntax

import (
	"cmp"
	"errors"
	"fmt"
	goast "go/ast"
	goparser "go/parser"
	"go/token"
	"go/types"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"

	"github.com/odvcencio/gotreesitter"
	golang "github.com/odvcencio/gotreesitter/grammars/go"
)

type goCall struct {
	line int
	name string
}

type repoGraph struct {
	src    []byte
	graph  Graph
	report Report
	err    error
}

const repoGraphWorkers = 8

var repoGraphs = sync.OnceValue(loadRepoGraphs)

func repoSourcePaths() ([]string, error) {
	var paths []string
	for _, dir := range []string{".", "../tool/search"} {
		found, err := filepath.Glob(filepath.Join(dir, "*.go"))
		if err != nil {
			return nil, err
		}
		paths = append(paths, found...)
	}
	return paths, nil
}

func loadRepoGraphs() map[string]repoGraph {
	lang, ok := ByName("go")
	if !ok {
		return map[string]repoGraph{"corpus": {err: errors.New("go language missing")}}
	}
	paths, err := repoSourcePaths()
	if err != nil {
		return map[string]repoGraph{"corpus": {err: err}}
	}
	out := make(map[string]repoGraph, len(paths))
	work := make(chan string)
	var mu sync.Mutex
	var wg sync.WaitGroup
	for range repoGraphWorkers {
		wg.Go(func() {
			for path := range work {
				res := parseRepoGraph(lang, path)
				mu.Lock()
				out[path] = res
				mu.Unlock()
			}
		})
	}
	for _, path := range paths {
		work <- path
	}
	close(work)
	wg.Wait()
	return out
}

func parseRepoGraph(lang Language, path string) repoGraph {
	src, err := os.ReadFile(path)
	if err != nil {
		return repoGraph{err: err}
	}
	parser := newParser(lang.l, 0)
	var tree *gotreesitter.Tree
	if factory := tokenSourceFactory(lang.Name(), parser.grammar); factory != nil {
		tree, err = parser.parser.ParseWithTokenSourceFactoryStrict(src, factory)
	} else {
		tree, err = parser.parser.ParseStrict(src)
	}
	if err != nil {
		tree.Release()
		return repoGraph{src: src, err: err}
	}
	defer tree.Release()
	query, err := lang.l.graphQuery()
	if err != nil {
		return repoGraph{src: src, err: err}
	}
	root := tree.RootNode()
	report := Report{SyntaxErrors: root.HasError(), ErrorLine: firstErrorLine(root)}
	graph, report, err := parser.graphOfTree(src, tree, query, report)
	return repoGraph{src: src, graph: graph, report: report, err: err}
}

func requireRepoGraph(t *testing.T, path string, res repoGraph) {
	t.Helper()
	if res.err != nil {
		t.Fatalf("%s: Graph: %v", path, res.err)
	}
	if res.report.Stopped || res.report.SyntaxErrors {
		t.Fatalf("%s: report = %+v, want clean parse", path, res.report)
	}
}

func graphOfSource(t *testing.T, langName, src string) (Graph, Report) {
	t.Helper()
	lang, ok := ByName(langName)
	if !ok {
		t.Fatalf("%s language missing", langName)
	}
	graph, report, err := lang.NewParser().Graph(t.Context(), []byte(src))
	if err != nil {
		t.Fatalf("Graph(%s): %v", langName, err)
	}
	return graph, report
}

func compareCalls(a, b goCall) int {
	return cmp.Or(cmp.Compare(a.line, b.line), cmp.Compare(a.name, b.name))
}

func compareImports(a, b Import) int {
	return cmp.Or(cmp.Compare(a.Line, b.Line), cmp.Compare(a.Spec, b.Spec))
}

func calleeIdent(fun goast.Expr) *goast.Ident {
	switch f := fun.(type) {
	case *goast.Ident:
		return f
	case *goast.SelectorExpr:
		return f.Sel
	case *goast.IndexExpr:
		return operandIdent(f.X)
	case *goast.IndexListExpr:
		return operandIdent(f.X)
	}
	return nil
}

func operandIdent(x goast.Expr) *goast.Ident {
	switch f := x.(type) {
	case *goast.Ident:
		return f
	case *goast.SelectorExpr:
		return f.Sel
	}
	return nil
}

func goParserCalls(t *testing.T, name string, src []byte) []goCall {
	t.Helper()
	fset := token.NewFileSet()
	file, err := goparser.ParseFile(fset, name, src, goparser.SkipObjectResolution)
	if err != nil {
		t.Fatalf("go/parser %s: %v", name, err)
	}
	var calls []goCall
	goast.Inspect(file, func(n goast.Node) bool {
		call, ok := n.(*goast.CallExpr)
		if !ok {
			return true
		}
		if ident := calleeIdent(call.Fun); ident != nil {
			calls = append(calls, goCall{line: fset.Position(ident.Pos()).Line, name: ident.Name})
		}
		return true
	})
	return calls
}

func graphCalls(graph Graph) []goCall {
	var calls []goCall
	for _, ref := range graph.Refs {
		if ref.Kind == RefCall {
			calls = append(calls, goCall{line: ref.Line, name: ref.Name})
		}
	}
	return calls
}

func callDiff(want, got []goCall) string {
	counts := make(map[goCall]int)
	for _, call := range want {
		counts[call]++
	}
	for _, call := range got {
		counts[call]--
	}
	var missing, extra []goCall
	for _, call := range slices.SortedFunc(maps.Keys(counts), compareCalls) {
		if counts[call] > 0 {
			missing = append(missing, call)
		}
		if counts[call] < 0 {
			extra = append(extra, call)
		}
	}
	if len(missing)+len(extra) == 0 {
		return ""
	}
	return fmt.Sprintf("missing from graph: %v\nextra in graph: %v", missing, extra)
}

func TestGraphGoCallsMatchGoParser(t *testing.T) {
	t.Parallel()
	all := repoGraphs()
	if len(all) < 10 {
		t.Fatalf("found %d Go files, want at least 10", len(all))
	}
	total := 0
	for _, path := range slices.Sorted(maps.Keys(all)) {
		res := all[path]
		requireRepoGraph(t, path, res)
		want := goParserCalls(t, path, res.src)
		if diff := callDiff(want, graphCalls(res.graph)); diff != "" {
			t.Fatalf("%s: RefCall refs differ from the go/parser calls\n%s", path, diff)
		}
		total += len(want)
	}
	if total == 0 {
		t.Fatal("the corpus holds no calls")
	}
	requireSelectorRowMatters(t)
}

func TestGraphGoGenericCallsMatchGoParser(t *testing.T) {
	t.Parallel()
	src := "package p\nfunc f() {\n" +
		"_, _ = errors.AsType[exitError](cause)\n" +
		"_ = local[Value](value)\n" +
		"_ = pair[Key, Value](key, value)\n" +
		"_ = remote.Pair[Key, Value](key, value)\n" +
		"_ = Value(value)\n" +
		"_ = remote.Value(value)\n" +
		"}\n"
	graph, report := graphOfSource(t, "go", src)
	if report.Stopped || report.SyntaxErrors {
		t.Fatalf("report = %+v, want clean parse", report)
	}
	want := goParserCalls(t, "generic.go", []byte(src))
	if diff := callDiff(want, graphCalls(graph)); diff != "" {
		t.Fatalf("generic calls differ from go/parser\n%s", diff)
	}
}

func requireSelectorRowMatters(t *testing.T) {
	t.Helper()
	var kept []string
	for line := range strings.SplitSeq(goGraph, "\n") {
		if !strings.Contains(line, "selector_expression") {
			kept = append(kept, line)
		}
	}
	lang := Language{l: newLanguage(languageSpec{
		name:       "go",
		extensions: []string{".go"},
		grammar:    golang.Language,
		query:      goQuery,
		graph:      strings.Join(kept, "\n"),
		ownerRules: goOwnerRules,
	})}
	src := []byte("package p\n\nfunc f() {\n\tobj.method()\n}\n")
	graph, _, err := lang.NewParser().Graph(t.Context(), src)
	if err != nil {
		t.Fatalf("Graph without selector rows: %v", err)
	}
	want := goParserCalls(t, "control.go", src)
	if len(want) != 1 || callDiff(want, graphCalls(graph)) == "" {
		t.Fatalf("graph without selector rows gives calls %v, go/parser gives %v, want a difference", graphCalls(graph), want)
	}
}

func goParserImports(t *testing.T, path string, src []byte) []Import {
	t.Helper()
	fset := token.NewFileSet()
	file, err := goparser.ParseFile(fset, path, src, goparser.ImportsOnly|goparser.SkipObjectResolution)
	if err != nil {
		t.Fatalf("go/parser %s: %v", path, err)
	}
	var out []Import
	for _, spec := range file.Imports {
		unquoted, unquoteErr := strconv.Unquote(spec.Path.Value)
		if unquoteErr != nil {
			t.Fatalf("%s: unquote %s: %v", path, spec.Path.Value, unquoteErr)
		}
		out = append(out, Import{Spec: unquoted, Line: fset.Position(spec.Path.Pos()).Line})
	}
	slices.SortFunc(out, compareImports)
	return slices.Compact(out)
}

func TestGraphGoImportsMatchGoParser(t *testing.T) {
	t.Parallel()
	all := repoGraphs()
	if len(all) < 10 {
		t.Fatalf("found %d Go files, want at least 10", len(all))
	}
	total := 0
	for _, path := range slices.Sorted(maps.Keys(all)) {
		res := all[path]
		requireRepoGraph(t, path, res)
		want := goParserImports(t, path, res.src)
		if !slices.Equal(want, res.graph.Imports) {
			t.Fatalf("%s: imports\nwant %v\ngot  %v", path, want, res.graph.Imports)
		}
		total += len(want)
	}
	if total == 0 {
		t.Fatal("the corpus holds no imports")
	}
}

func typeVarsSource(names []string, prefix string) string {
	var b strings.Builder
	b.WriteString("package p\n\n")
	for i, name := range names {
		fmt.Fprintf(&b, "var v%d %s%s\n", i, prefix, name)
	}
	return b.String()
}

func predeclaredTypeNames(t *testing.T) []string {
	t.Helper()
	var names []string
	for _, name := range types.Universe.Names() {
		if _, ok := types.Universe.Lookup(name).(*types.TypeName); ok {
			names = append(names, name)
		}
	}
	if len(names) < 15 {
		t.Fatalf("universe holds %d type names, want at least 15", len(names))
	}
	return names
}

func TestGraphDropsPredeclaredGoTypes(t *testing.T) {
	t.Parallel()
	predeclared := predeclaredTypeNames(t)
	dropped, _ := graphOfSource(t, "go", typeVarsSource(predeclared, ""))
	if len(dropped.Refs) != 0 {
		t.Fatalf("predeclared types kept as refs: %v", dropped.Refs)
	}
	renamed, _ := graphOfSource(t, "go", typeVarsSource(predeclared, "X"))
	requireRenamedTypeRefs(t, renamed, predeclared)
	builtins, _ := graphOfSource(t, "go", typeVarsSource([]string{"len", "append"}, ""))
	if len(builtins.Refs) != 2 || builtins.Refs[0].Name != "len" || builtins.Refs[1].Name != "append" {
		t.Fatalf("universe names that are not types gave refs %v, want len and append", builtins.Refs)
	}
	requireOnlyForeignTypeKept(t)
	requirePredeclaredFilterIsGoOnly(t)
}

func requireRenamedTypeRefs(t *testing.T, graph Graph, names []string) {
	t.Helper()
	if len(graph.Refs) != len(names) {
		t.Fatalf("renamed types give %d refs, want %d", len(graph.Refs), len(names))
	}
	for i, ref := range graph.Refs {
		if ref.Kind != RefType || ref.Name != "X"+names[i] || ref.Line != i+3 {
			t.Fatalf("ref %d = %+v, want a type ref %q on line %d", i, ref, "X"+names[i], i+3)
		}
	}
}

func requireOnlyForeignTypeKept(t *testing.T) {
	t.Helper()
	graph, _ := graphOfSource(t, "go", "package p\n\nvar a int\nvar b Foo\n")
	if len(graph.Refs) != 1 || graph.Refs[0].Kind != RefType || graph.Refs[0].Name != "Foo" || graph.Refs[0].Line != 4 {
		t.Fatalf("refs = %+v, want the single type ref Foo on line 4", graph.Refs)
	}
}

func requirePredeclaredFilterIsGoOnly(t *testing.T) {
	t.Helper()
	graph, _ := graphOfSource(t, "python", "def f(a: int):\n    pass\n")
	if len(graph.Refs) != 1 || graph.Refs[0].Kind != RefType || graph.Refs[0].Name != "int" {
		t.Fatalf("python refs = %+v, want the type ref int", graph.Refs)
	}
}

func TestGraphQueriesCompile(t *testing.T) {
	t.Parallel()
	graphNames := []string{"go", "java", "python", "rust", "typescript"}
	want := []string{"call", "import", "type"}
	for _, name := range graphNames {
		lang, ok := ByName(name)
		if !ok {
			t.Fatalf("%s language missing", name)
		}
		if !lang.HasGraph() {
			t.Fatalf("%s has no graph", name)
		}
		query, err := lang.l.graphQuery()
		if err != nil {
			t.Fatalf("%s graph query: %v", name, err)
		}
		if got := query.CaptureNames(); !slices.Equal(got, want) {
			t.Fatalf("%s graph captures = %v, want %v", name, got, want)
		}
	}
	lang, _ := ByName("go")
	if _, err := lang.Compile("(call_expresion function: (identifier) @call)"); err == nil {
		t.Fatal("a misspelled node type compiled")
	}
	for _, name := range Names() {
		if slices.Contains(graphNames, name) {
			continue
		}
		requireNoGraph(t, name)
	}
}

func requireNoGraph(t *testing.T, name string) {
	t.Helper()
	lang, _ := ByName(name)
	if lang.HasGraph() {
		t.Fatalf("%s has a graph", name)
	}
	_, _, err := lang.NewParser().Graph(t.Context(), nil)
	if want := "syntax: no graph for " + name; err == nil || err.Error() != want {
		t.Fatalf("%s Graph error = %v, want %q", name, err, want)
	}
}
