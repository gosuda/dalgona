package syntax

import (
	"fmt"
	"math/rand/v2"
	"slices"
	"strings"
	"testing"
)

type graphProgram struct {
	lang   string
	header string
	open   string
	body   string
	close  string
	owner  string
	spec   string
}

func TestGraphMatchesGeneratedPrograms(t *testing.T) {
	t.Parallel()
	programs := []graphProgram{
		{"go", "package p\nimport \"lib/x\"\n", "", "func caller%d(x Widget%d) { peer%d() }\n", "", "", "lib/x"},
		{"typescript", "import { x } from \"lib/x\";\n", "", "function caller%d(x: Widget%d) { peer%d(); }\n", "", "", "lib/x"},
		{"python", "from lib.x import x\n", "", "def caller%d(x: Widget%d):\n    peer%d()\n", "", "", "lib.x"},
		{"rust", "use lib::x;\n", "", "fn caller%d(x: Widget%d) { peer%d(); }\n", "", "", "lib::x"},
		{"java", "import lib.X;\n", "class Host {\n", "void caller%d(Widget%d x) { peer%d(); }\n", "}\n", "Host.", "lib.X"},
	}
	for _, program := range programs {
		t.Run(program.lang, func(t *testing.T) { checkGeneratedGraph(t, program) })
	}
}

func checkGeneratedGraph(t *testing.T, program graphProgram) {
	t.Helper()
	source, want := generatedProgram(program)
	graph, report := graphOfSource(t, program.lang, source)
	if report.Stopped || report.SyntaxErrors {
		t.Fatalf("parse report: %+v", report)
	}
	requireRefLedger(t, graph, want)
	line := 1
	if program.lang == "go" {
		line = 2
	}
	if !slices.Equal(graph.Imports, []Import{{Spec: program.spec, Line: line}}) {
		t.Fatalf("imports = %v, want %s at %d", graph.Imports, program.spec, line)
	}
}

func generatedProgram(program graphProgram) (string, []string) {
	var source strings.Builder
	source.WriteString(program.header)
	source.WriteString(program.open)
	var want []string
	for i := range 12 {
		line := strings.Count(source.String(), "\n") + 1
		fmt.Fprintf(&source, program.body, i, i, i)
		caller := fmt.Sprintf("%scaller%d", program.owner, i)
		want = append(want, fmt.Sprintf("%d type Widget%d %s", line, i, caller))
		if program.lang == "python" {
			line++
		}
		want = append(want, fmt.Sprintf("%d call peer%d %s", line, i, caller))
	}
	source.WriteString(program.close)
	return source.String(), want
}

func requireRefLedger(t *testing.T, graph Graph, want []string) {
	t.Helper()
	var got []string
	for _, ref := range graph.Refs {
		if ref.Scope < 0 || ref.Scope >= len(graph.Symbols) {
			t.Fatalf("invalid scope: %+v", ref)
		}
		got = append(got, fmt.Sprintf("%d %s %s %s", ref.Line, ref.Kind, ref.Name, graph.Symbols[ref.Scope].Qualified()))
	}
	slices.Sort(want)
	slices.Sort(got)
	if !slices.Equal(got, want) {
		t.Fatalf("reference ledger mismatch\nwant %v\ngot %v", want, got)
	}
}

func TestRustUseSpecsExpand(t *testing.T) {
	t.Parallel()
	rng := rand.New(rand.NewPCG(47, 369))
	for range 100 {
		tree, want := generatedUse(rng, "root", 0)
		got := importSpecs("rust", "pub(crate) use "+tree+";")
		slices.Sort(want)
		slices.Sort(got)
		if !slices.Equal(got, want) {
			t.Fatalf("%s: got %v, want %v", tree, got, want)
		}
	}
}

func generatedUse(rng *rand.Rand, prefix string, depth int) (string, []string) {
	if depth == 3 {
		return prefix + " as alias", []string{prefix}
	}
	var branches []string
	want := []string{prefix}
	branches = append(branches, "self")
	for i := range 1 + rng.IntN(3) {
		name := fmt.Sprintf("n%d", i)
		child, leaves := generatedUse(rng, prefix+"::"+name, depth+1)
		branches = append(branches, strings.TrimPrefix(child, prefix+"::"))
		want = append(want, leaves...)
	}
	return prefix + "::{" + strings.Join(branches, ", ") + "}", want
}

func TestGraphHalfOpenScopesAndDefinitionNames(t *testing.T) {
	t.Parallel()
	graph, _ := graphOfSource(t, "go", "package p\nfunc outer() {\n type Local struct {}\n var value Local\n peer()\n}\nvar top = peer()\n")
	var calls []string
	for _, ref := range graph.Refs {
		if ref.Name == "Local" && ref.Line == 3 {
			t.Fatal("local definition name became a reference")
		}
		if ref.Kind == RefCall {
			calls = append(calls, graph.Symbols[ref.Scope].Qualified())
		}
	}
	if !slices.Equal(calls, []string{"outer", "top"}) {
		t.Fatalf("scopes = %v", calls)
	}
	symbols := []outlinedSymbol{{start: 0, end: 20}, {start: 5, end: 10}, {start: 5, end: 10}}
	for _, test := range []struct{ pos, want int }{{5, 1}, {9, 1}, {10, 0}, {20, -1}} {
		if got := innermostSymbol(symbols, test.pos); got != test.want {
			t.Fatalf("scope at %d = %d, want %d", test.pos, got, test.want)
		}
	}
}
