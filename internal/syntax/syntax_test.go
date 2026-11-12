package syntax

import (
	"context"
	"errors"
	"fmt"
	"math/rand/v2"
	"os"
	"slices"
	"strings"
	"testing"
	"time"
)

func TestForPathExtensionTable(t *testing.T) {
	t.Parallel()
	cases := []struct {
		path string
		name string
		ok   bool
	}{
		{path: "main.go", name: "go", ok: true},
		{path: "MAIN.GO", name: "go", ok: true},
		{path: "main.txt", ok: false},
		{path: "Makefile", ok: false},
	}
	for _, c := range cases {
		lang, ok := ForPath(c.path)
		if ok != c.ok {
			t.Fatalf("ForPath(%q) ok = %v, want %v", c.path, ok, c.ok)
		}
		if ok && lang.Name() != c.name {
			t.Fatalf("ForPath(%q) = %q, want %q", c.path, lang.Name(), c.name)
		}
	}
	if _, ok := ByName("go"); !ok {
		t.Fatal("ByName(go) not found")
	}
	if _, ok := ByName("nope"); ok {
		t.Fatal("ByName(nope) found")
	}
	if got := Names(); !slices.Equal(got, []string{"go"}) {
		t.Fatalf("Names() = %v, want [go]", got)
	}
}

func TestKindTable(t *testing.T) {
	t.Parallel()
	cases := map[string]Kind{
		"function":    "function",
		"method":      "method",
		"class":       "class",
		"interface":   "interface",
		"type":        "type",
		"enum":        "enum",
		"constant":    "constant",
		"variable":    "variable",
		"module":      "module",
		"macro":       "macro",
		"section":     "section",
		"constructor": "method",
		"record":      "class",
	}
	for library, want := range cases {
		got, err := kindOf(library)
		if err != nil {
			t.Fatalf("kindOf(%q) error %v", library, err)
		}
		if got != want {
			t.Fatalf("kindOf(%q) = %q, want %q", library, got, want)
		}
	}
}

func TestKindRejectsOutsideSet(t *testing.T) {
	t.Parallel()
	_, err := kindOf("struct")
	if err == nil || err.Error() != "syntax: unknown kind struct" {
		t.Fatalf("kindOf(struct) error = %v, want syntax: unknown kind struct", err)
	}
}

func TestSymbolsGoFixture(t *testing.T) {
	t.Parallel()
	src, err := os.ReadFile("testdata/go/definitions.go")
	if err != nil {
		t.Fatal(err)
	}
	lang, ok := ByName("go")
	if !ok {
		t.Fatal("go language missing")
	}
	symbols, report, err := lang.NewParser().Symbols(t.Context(), src)
	if err != nil {
		t.Fatalf("Symbols: %v", err)
	}
	if report.Stopped || report.SyntaxErrors {
		t.Fatalf("report = %+v, want clean parse", report)
	}
	want := []Symbol{
		{Kind: "type", Name: "Shape", Start: 3, End: 5, Depth: 0},
		{Kind: "interface", Name: "Named", Start: 7, End: 9, Depth: 0},
		{Kind: "method", Name: "Name", Start: 8, End: 8, Depth: 1},
		{Kind: "type", Name: "Count", Start: 11, End: 11, Depth: 0},
		{Kind: "type", Name: "Alias", Start: 13, End: 13, Depth: 0},
		{Kind: "constant", Name: "Limit", Start: 15, End: 15, Depth: 0},
		{Kind: "variable", Name: "Default", Start: 17, End: 17, Depth: 0},
		{Kind: "function", Name: "Area", Start: 19, End: 22, Depth: 0},
		{Kind: "method", Name: "Double", Start: 24, End: 26, Depth: 0},
	}
	if !slices.Equal(symbols, want) {
		t.Fatalf("symbols =\n%v\nwant\n%v", symbols, want)
	}
}

func TestSymbolsLocalDefinitionsOmitted(t *testing.T) {
	t.Parallel()
	lang, _ := ByName("go")
	src := []byte("func F() {\n\tvar x = 1\n\t_ = x\n}\n")
	symbols, _, err := lang.NewParser().Symbols(t.Context(), src)
	if err != nil {
		t.Fatalf("Symbols: %v", err)
	}
	want := []Symbol{{Kind: "function", Name: "F", Start: 1, End: 4, Depth: 0}}
	if !slices.Equal(symbols, want) {
		t.Fatalf("symbols = %v, want %v", symbols, want)
	}
}

func TestSymbolsRejectsContextCancelled(t *testing.T) {
	t.Parallel()
	lang, _ := ByName("go")
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	_, _, err := lang.NewParser().Symbols(ctx, []byte("package p\n"))
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("Symbols error = %v, want context.Canceled", err)
	}
}

func TestSymbolsStoppedWithTimeout(t *testing.T) {
	t.Parallel()
	rng := rand.New(rand.NewPCG(1, 2))
	var b strings.Builder
	b.WriteString("package p\n")
	for b.Len() < 1<<20 {
		fmt.Fprintf(&b, "func f%06d() int { return %06d }\n", rng.IntN(1000000), rng.IntN(1000000))
	}
	p := newParser(registry[0], time.Microsecond)
	symbols, report, err := p.Symbols(t.Context(), []byte(b.String()))
	if err != nil {
		t.Fatalf("Symbols: %v", err)
	}
	if !report.Stopped {
		t.Fatalf("report = %+v, want Stopped", report)
	}
	if len(symbols) != 0 {
		t.Fatalf("symbols = %d, want none after a stopped parse", len(symbols))
	}
}
