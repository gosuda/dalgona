package syntax

import (
	"context"
	"errors"
	"slices"
	"strings"
	"testing"
	"time"

	golang "github.com/odvcencio/gotreesitter/grammars/go"
)

func TestGraphStoppedAndCancelled(t *testing.T) {
	t.Parallel()
	lang, _ := ByName("go")
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	_, _, err := lang.NewParser().Graph(ctx, []byte("package p\n"))
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("canceled Graph: %v", err)
	}
	parser := newParser(lang.l, time.Nanosecond)
	graph, report, err := parser.Graph(t.Context(), []byte("package p\nfunc f() { peer() }\n"))
	if err != nil || !report.Stopped || len(graph.Symbols)+len(graph.Refs)+len(graph.Imports) != 0 {
		t.Fatalf("stopped Graph: %+v, %+v, %v", graph, report, err)
	}
	clean, report := graphOfSource(t, "go", "package p\nfunc f() { peer() }\n")
	if report.Stopped || len(clean.Refs) != 1 {
		t.Fatalf("normal parse control: %+v, %+v", clean, report)
	}
}

func TestGraphInternalQueryFailureIsError(t *testing.T) {
	t.Parallel()
	lang := Language{l: newLanguage(languageSpec{name: "go", grammar: golang.Language, query: goQuery, graph: "(call_expresion) @call"})}
	_, report, err := lang.NewParser().Graph(t.Context(), []byte("package p\n"))
	if err == nil || !strings.HasPrefix(err.Error(), "syntax:") || report.Stopped {
		t.Fatalf("invalid query: report %+v, error %v", report, err)
	}
}

func TestGraphImportsNormalize(t *testing.T) {
	t.Parallel()
	cases := []struct {
		lang string
		text string
		want []string
	}{
		{"go", "`example.org/x`", []string{"example.org/x"}},
		{"typescript", "'./x'", []string{"./x"}},
		{"python", "..tools", []string{"..tools"}},
		{"java", "import static java.util.Collections.*;", []string{"java.util.Collections.*"}},
		{"rust", "pub use a::{b, c::{d, e as f}, self};", []string{"a::b", "a::c::d", "a::c::e", "a"}},
	}
	for _, test := range cases {
		if got := importSpecs(test.lang, test.text); !slices.Equal(got, test.want) {
			t.Fatalf("%s %s: got %v, want %v", test.lang, test.text, got, test.want)
		}
	}
}

func singleGraphRef(t *testing.T, lang, src, scenario string) Ref {
	t.Helper()
	graph, report := graphOfSource(t, lang, src)
	if report.Stopped || report.SyntaxErrors || len(graph.Refs) != 1 {
		t.Fatalf("%s: graph %+v, report %+v", scenario, graph, report)
	}
	return graph.Refs[0]
}

func TestGraphKeepsTopLevelAndDeduplicatesCalls(t *testing.T) {
	t.Parallel()
	if ref := singleGraphRef(t, "python", "peer()\n", "top-level graph"); ref.Scope != -1 {
		t.Fatalf("top-level scope = %d, want -1", ref.Scope)
	}
	ref := singleGraphRef(t, "java", "class Host { void run() { new Widget(); } }\n", "constructor call/type overlap")
	if ref.Kind != RefCall || ref.Name != "Widget" {
		t.Fatalf("constructor ref = %+v, want call Widget", ref)
	}
}
