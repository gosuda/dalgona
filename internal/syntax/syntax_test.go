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

	"github.com/odvcencio/gotreesitter"
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
}

func TestNamesListsAllLanguages(t *testing.T) {
	t.Parallel()
	want := []string{"bash", "c", "c_sharp", "cpp", "css", "go", "html", "java", "javascript", "json", "kotlin", "lua", "markdown", "php", "python", "ruby", "rust", "svelte", "swift", "toml", "tsx", "typescript", "yaml"}
	if got := Names(); !slices.Equal(got, want) {
		t.Fatalf("Names() = %v, want the 23 registry names", got)
	}
}

func TestForPathEveryExtension(t *testing.T) {
	t.Parallel()
	cases := map[string][]string{
		"bash":       {".sh"},
		"c":          {".c", ".h"},
		"c_sharp":    {".cs"},
		"cpp":        {".cc", ".cpp", ".cxx", ".hpp", ".hh", ".hxx"},
		"css":        {".css"},
		"go":         {".go"},
		"html":       {".html", ".htm"},
		"java":       {".java"},
		"javascript": {".js", ".jsx", ".mjs", ".cjs"},
		"json":       {".json"},
		"kotlin":     {".kt", ".kts"},
		"lua":        {".lua"},
		"markdown":   {".md"},
		"php":        {".php"},
		"python":     {".py"},
		"ruby":       {".rb"},
		"rust":       {".rs"},
		"svelte":     {".svelte"},
		"swift":      {".swift"},
		"toml":       {".toml"},
		"tsx":        {".tsx"},
		"typescript": {".ts", ".mts", ".cts"},
		"yaml":       {".yaml", ".yml"},
	}
	for name, exts := range cases {
		for _, ext := range exts {
			lang, ok := ForPath("file" + ext)
			if !ok {
				t.Fatalf("ForPath(file%s) found no language; want %q", ext, name)
			}
			if lang.Name() != name {
				t.Fatalf("ForPath(file%s) = %q; want %q", ext, lang.Name(), name)
			}
		}
	}
}

func TestSyntaxOnlyGrammarsParse(t *testing.T) {
	t.Parallel()
	samples := map[string]string{
		"svelte": "testdata/svelte/sample.svelte",
		"html":   "testdata/html/sample.html",
		"css":    "testdata/css/sample.css",
		"json":   "testdata/json/sample.json",
		"yaml":   "testdata/yaml/sample.yaml",
		"toml":   "testdata/toml/sample.toml",
	}
	for name, path := range samples {
		src, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		lang, ok := ByName(name)
		if !ok {
			t.Fatalf("%s language missing", name)
		}
		tree, report, err := lang.NewParser().parse(t.Context(), src)
		if err != nil || report.Stopped || report.SyntaxErrors {
			t.Fatalf("%s parse = %+v, %v; want clean", name, report, err)
		}
		tree.Release()
	}
}

func TestSyntaxOnlySymbolsRefused(t *testing.T) {
	t.Parallel()
	lang, ok := ByName("svelte")
	if !ok {
		t.Fatal("svelte language missing")
	}
	src, err := os.ReadFile("testdata/svelte/sample.svelte")
	if err != nil {
		t.Fatal(err)
	}
	_, _, err = lang.NewParser().Symbols(t.Context(), src)
	if err == nil || err.Error() != "syntax: no definition query for svelte" {
		t.Fatalf("Symbols error = %v, want syntax: no definition query for svelte", err)
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

func assertFixtureSymbols(t *testing.T, langName, path string, want []Symbol) {
	t.Helper()
	src, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	lang, ok := ByName(langName)
	if !ok {
		t.Fatalf("%s language missing", langName)
	}
	symbols, report, err := lang.NewParser().Symbols(t.Context(), src)
	if err != nil {
		t.Fatalf("Symbols: %v", err)
	}
	if report.Stopped || report.SyntaxErrors {
		t.Fatalf("report = %+v, want clean parse", report)
	}
	if !slices.Equal(symbols, want) {
		t.Fatalf("symbols =\n%v\nwant\n%v", symbols, want)
	}
}

func TestSymbolsFixtureTypeScript(t *testing.T) {
	t.Parallel()
	assertFixtureSymbols(t, "typescript", "testdata/typescript/definitions.ts", []Symbol{
		{Kind: "function", Name: "greet", Start: 1, End: 3, Depth: 0},
		{Kind: "class", Name: "Box", Start: 5, End: 8, Depth: 0},
		{Kind: "method", Name: "open", Start: 6, End: 6, Depth: 1},
		{Kind: "method", Name: "seal", Start: 7, End: 7, Depth: 1},
		{Kind: "class", Name: "Base", Start: 10, End: 12, Depth: 0},
		{Kind: "interface", Name: "Shape", Start: 14, End: 16, Depth: 0},
		{Kind: "enum", Name: "Color", Start: 18, End: 20, Depth: 0},
		{Kind: "type", Name: "Pair", Start: 22, End: 22, Depth: 0},
		{Kind: "function", Name: "twice", Start: 24, End: 24, Depth: 0},
		{Kind: "module", Name: "Util", Start: 26, End: 28, Depth: 0},
	})
}

func TestSymbolsFixtureTSX(t *testing.T) {
	t.Parallel()
	assertFixtureSymbols(t, "tsx", "testdata/tsx/definitions.tsx", []Symbol{
		{Kind: "function", Name: "Card", Start: 1, End: 3, Depth: 0},
		{Kind: "function", Name: "Button", Start: 5, End: 5, Depth: 0},
		{Kind: "interface", Name: "CardProps", Start: 7, End: 9, Depth: 0},
	})
}

func TestSymbolsFixtureJavaScript(t *testing.T) {
	t.Parallel()
	assertFixtureSymbols(t, "javascript", "testdata/javascript/definitions.js", []Symbol{
		{Kind: "function", Name: "add", Start: 1, End: 3, Depth: 0},
		{Kind: "class", Name: "Counter", Start: 5, End: 10, Depth: 0},
		{Kind: "method", Name: "bump", Start: 6, End: 8, Depth: 1},
		{Kind: "method", Name: "#hide", Start: 9, End: 9, Depth: 1},
		{Kind: "function", Name: "scale", Start: 12, End: 12, Depth: 0},
		{Kind: "function", Name: "gen", Start: 14, End: 16, Depth: 0},
	})
}

func TestSymbolsFixturePython(t *testing.T) {
	t.Parallel()
	assertFixtureSymbols(t, "python", "testdata/python/definitions.py", []Symbol{
		{Kind: "function", Name: "load", Start: 1, End: 2, Depth: 0},
		{Kind: "class", Name: "Store", Start: 5, End: 9, Depth: 0},
		{Kind: "method", Name: "get", Start: 6, End: 9, Depth: 1},
		{Kind: "function", Name: "fetch", Start: 11, End: 13, Depth: 0},
	})
}

func TestSymbolsFixtureRust(t *testing.T) {
	t.Parallel()
	assertFixtureSymbols(t, "rust", "testdata/rust/definitions.rs", []Symbol{
		{Kind: "function", Name: "start", Start: 1, End: 1, Depth: 0},
		{Kind: "type", Name: "Config", Start: 3, End: 5, Depth: 0},
		{Kind: "enum", Name: "Mode", Start: 7, End: 9, Depth: 0},
		{Kind: "interface", Name: "Runner", Start: 11, End: 13, Depth: 0},
		{Kind: "method", Name: "run", Start: 12, End: 12, Depth: 1},
		{Kind: "method", Name: "new", Start: 16, End: 18, Depth: 0},
	})
}

func TestSymbolsFixtureJava(t *testing.T) {
	t.Parallel()
	assertFixtureSymbols(t, "java", "testdata/java/Greeter.java", []Symbol{
		{Kind: "class", Name: "Greeter", Start: 1, End: 4, Depth: 0},
		{Kind: "method", Name: "Greeter", Start: 2, End: 2, Depth: 1},
		{Kind: "method", Name: "hi", Start: 3, End: 3, Depth: 1},
		{Kind: "interface", Name: "Named", Start: 5, End: 5, Depth: 0},
		{Kind: "enum", Name: "Mode", Start: 6, End: 6, Depth: 0},
		{Kind: "class", Name: "Point", Start: 7, End: 7, Depth: 0},
	})
}

func TestSymbolsFixtureC(t *testing.T) {
	t.Parallel()
	assertFixtureSymbols(t, "c", "testdata/c/sample.c", []Symbol{
		{Kind: "type", Name: "Pt", Start: 1, End: 3, Depth: 0},
		{Kind: "type", Name: "Count", Start: 4, End: 4, Depth: 0},
		{Kind: "function", Name: "add", Start: 5, End: 7, Depth: 0},
		{Kind: "function", Name: "label", Start: 8, End: 10, Depth: 0},
	})
}

func TestSymbolsFixtureCPP(t *testing.T) {
	t.Parallel()
	assertFixtureSymbols(t, "cpp", "testdata/cpp/sample.cpp", []Symbol{
		{Kind: "function", Name: "add", Start: 1, End: 3, Depth: 0},
		{Kind: "class", Name: "Box", Start: 4, End: 7, Depth: 0},
		{Kind: "method", Name: "open", Start: 6, End: 6, Depth: 1},
		{Kind: "method", Name: "reset", Start: 8, End: 8, Depth: 0},
		{Kind: "module", Name: "util", Start: 9, End: 10, Depth: 0},
	})
}

func TestSymbolsFixtureCSharp(t *testing.T) {
	t.Parallel()
	assertFixtureSymbols(t, "c_sharp", "testdata/c_sharp/Sample.cs", []Symbol{
		{Kind: "class", Name: "Greeter", Start: 1, End: 4, Depth: 0},
		{Kind: "method", Name: "Greeter", Start: 2, End: 2, Depth: 1},
		{Kind: "method", Name: "Hi", Start: 3, End: 3, Depth: 1},
		{Kind: "interface", Name: "INamed", Start: 5, End: 5, Depth: 0},
		{Kind: "enum", Name: "Mode", Start: 6, End: 6, Depth: 0},
		{Kind: "type", Name: "Pt", Start: 7, End: 7, Depth: 0},
		{Kind: "class", Name: "Dot", Start: 8, End: 8, Depth: 0},
	})
}

func TestSymbolsFixtureKotlin(t *testing.T) {
	t.Parallel()
	assertFixtureSymbols(t, "kotlin", "testdata/kotlin/Sample.kt", []Symbol{
		{Kind: "class", Name: "Box", Start: 1, End: 3, Depth: 0},
		{Kind: "method", Name: "open", Start: 2, End: 2, Depth: 1},
		{Kind: "interface", Name: "Named", Start: 4, End: 4, Depth: 0},
		{Kind: "enum", Name: "Mode", Start: 5, End: 5, Depth: 0},
		{Kind: "constant", Name: "A", Start: 5, End: 5, Depth: 1},
		{Kind: "constant", Name: "B", Start: 5, End: 5, Depth: 1},
		{Kind: "class", Name: "Store", Start: 6, End: 6, Depth: 0},
		{Kind: "type", Name: "Id", Start: 7, End: 7, Depth: 0},
	})
}

func symbolsWithin(t *testing.T, langName string, src []byte) ([]Symbol, Report, error) {
	t.Helper()
	lang, ok := ByName(langName)
	if !ok {
		t.Fatalf("%s language missing", langName)
	}
	if _, err := lang.l.outliner(); err != nil {
		t.Fatalf("outliner %s: %v", langName, err)
	}
	return symbolsFrom(t, lang.NewParser(), src)
}

func symbolsFrom(t *testing.T, parser *Parser, src []byte) ([]Symbol, Report, error) {
	t.Helper()
	type outcome struct {
		symbols []Symbol
		report  Report
		err     error
	}
	done := make(chan outcome, 1)
	go func() {
		symbols, report, err := parser.Symbols(t.Context(), src)
		done <- outcome{symbols, report, err}
	}()
	select {
	case got := <-done:
		return got.symbols, got.report, got.err
	case <-time.After(10 * time.Second):
		t.Fatal("Symbols did not return within 10s")
		return nil, Report{}, nil
	}
}

func TestSymbolsCPPIssue1558(t *testing.T) {
	t.Parallel()
	lang, ok := ByName("cpp")
	if !ok {
		t.Fatal("cpp language missing")
	}
	symbols, report, err := symbolsFrom(t, newParser(lang.l, 0), []byte(";e s d>f(T);template"))
	if err != nil {
		t.Fatalf("Symbols: %v", err)
	}
	if !report.Stopped {
		t.Fatalf("report = %+v, want Stopped", report)
	}
	if len(symbols) != 0 {
		t.Fatalf("symbols = %d, want none", len(symbols))
	}
}

func TestSymbolsOneMiBSingleLine(t *testing.T) {
	t.Parallel()
	src := "package p\nvar s = \"" + strings.Repeat("a", 1<<20)
	_, report, err := symbolsWithin(t, "go", []byte(src))
	if err != nil {
		t.Fatalf("Symbols: %v", err)
	}
	if !report.Stopped && !report.SyntaxErrors {
		t.Fatalf("report = %+v, want Stopped or SyntaxErrors", report)
	}
}

func TestSymbolsDeeplyNestedBrackets(t *testing.T) {
	t.Parallel()
	src := "package p\n" + strings.Repeat("(", 200_000)
	_, report, err := symbolsWithin(t, "go", []byte(src))
	if err != nil {
		t.Fatalf("Symbols: %v", err)
	}
	if !report.Stopped && !report.SyntaxErrors {
		t.Fatalf("report = %+v, want Stopped or SyntaxErrors", report)
	}
}

type fixtureCase struct {
	lang string
	path string
	want []Symbol
}

var fixtureCases = map[string]fixtureCase{
	"swift": {"swift", "testdata/swift/definitions.swift", []Symbol{
		{Kind: "interface", Name: "Named", Start: 1, End: 3, Depth: 0},
		{Kind: "method", Name: "name", Start: 2, End: 2, Depth: 1},
		{Kind: "class", Name: "Box", Start: 4, End: 6, Depth: 0},
		{Kind: "method", Name: "open", Start: 5, End: 5, Depth: 1},
		{Kind: "type", Name: "Pt", Start: 7, End: 9, Depth: 0},
		{Kind: "enum", Name: "Mode", Start: 10, End: 12, Depth: 0},
		{Kind: "constant", Name: "fast", Start: 11, End: 11, Depth: 1},
		{Kind: "type", Name: "Id", Start: 13, End: 13, Depth: 0},
	}},
	"ruby": {"ruby", "testdata/ruby/definitions.rb", []Symbol{
		{Kind: "module", Name: "Tools", Start: 1, End: 9, Depth: 0},
		{Kind: "constant", Name: "LIMIT", Start: 2, End: 2, Depth: 1},
		{Kind: "class", Name: "Runner", Start: 3, End: 6, Depth: 1},
		{Kind: "method", Name: "run", Start: 4, End: 5, Depth: 2},
		{Kind: "method", Name: "build", Start: 7, End: 8, Depth: 1},
		{Kind: "function", Name: "top", Start: 10, End: 11, Depth: 0},
		{Kind: "function", Name: "m", Start: 12, End: 13, Depth: 0},
		{Kind: "class", Name: "Store::Vault", Start: 14, End: 17, Depth: 0},
		{Kind: "method", Name: "name=", Start: 15, End: 16, Depth: 1},
	}},
	"php": {"php", "testdata/php/definitions.php", []Symbol{
		{Kind: "function", Name: "helper", Start: 2, End: 2, Depth: 0},
		{Kind: "class", Name: "Box", Start: 3, End: 5, Depth: 0},
		{Kind: "method", Name: "open", Start: 4, End: 4, Depth: 1},
		{Kind: "interface", Name: "Named", Start: 6, End: 6, Depth: 0},
		{Kind: "interface", Name: "Logs", Start: 7, End: 7, Depth: 0},
		{Kind: "enum", Name: "Mode", Start: 8, End: 10, Depth: 0},
		{Kind: "constant", Name: "Fast", Start: 9, End: 9, Depth: 1},
		{Kind: "constant", Name: "LIMIT", Start: 11, End: 11, Depth: 0},
	}},
	"bash": {"bash", "testdata/bash/definitions.sh", []Symbol{
		{Kind: "function", Name: "greet", Start: 1, End: 3, Depth: 0},
		{Kind: "function", Name: "run_all", Start: 4, End: 6, Depth: 0},
	}},
	"lua": {"lua", "testdata/lua/definitions.lua", []Symbol{
		{Kind: "function", Name: "helper", Start: 1, End: 3, Depth: 0},
		{Kind: "method", Name: "open", Start: 4, End: 6, Depth: 0},
		{Kind: "method", Name: "close", Start: 7, End: 9, Depth: 0},
		{Kind: "function", Name: "run", Start: 10, End: 11, Depth: 0},
	}},
	"markdown": {"markdown", "testdata/markdown/definitions.md", []Symbol{
		{Kind: "section", Name: "Title", Start: 1, End: 10, Depth: 0},
		{Kind: "section", Name: "Setup", Start: 5, End: 10, Depth: 1},
	}},
}

func assertFixtureCase(t *testing.T, name string) {
	t.Helper()
	c := fixtureCases[name]
	assertFixtureSymbols(t, c.lang, c.path, c.want)
}

func TestSymbolsFixtureSwift(t *testing.T) {
	t.Parallel()
	assertFixtureCase(t, "swift")
}

func TestSymbolsFixtureRuby(t *testing.T) {
	t.Parallel()
	assertFixtureCase(t, "ruby")
}

func TestSymbolsFixturePHP(t *testing.T) {
	t.Parallel()
	assertFixtureCase(t, "php")
}

func TestSymbolsFixtureBash(t *testing.T) {
	t.Parallel()
	assertFixtureCase(t, "bash")
}

func TestSymbolsFixtureLua(t *testing.T) {
	t.Parallel()
	assertFixtureCase(t, "lua")
}

func TestSymbolsFixtureMarkdown(t *testing.T) {
	t.Parallel()
	assertFixtureCase(t, "markdown")
}

func TestCaptureNamesSortedDistinct(t *testing.T) {
	t.Parallel()
	lang, ok := ByName("go")
	if !ok {
		t.Fatal("go language missing")
	}
	q, err := lang.Compile("(function_declaration name: (identifier) @name) @definition.function")
	if err != nil {
		t.Fatalf("Compile: %v", err)
	}
	want := []string{"definition.function", "name"}
	if got := q.CaptureNames(); !slices.Equal(got, want) {
		t.Fatalf("CaptureNames() = %v, want %v", got, want)
	}
}

func TestErrorLineOfBrokenGo(t *testing.T) {
	t.Parallel()
	_, report, err := symbolsWithin(t, "go", []byte("package a\n\nfunc Bad( {\n"))
	if err != nil {
		t.Fatalf("Symbols: %v", err)
	}
	if !report.SyntaxErrors || report.ErrorLine != 3 {
		t.Fatalf("report = %+v, want SyntaxErrors at line 3", report)
	}
}

func TestErrorLineZeroOnValidGo(t *testing.T) {
	t.Parallel()
	_, report, err := symbolsWithin(t, "go", []byte("package a\n\nfunc Good() {}\n"))
	if err != nil {
		t.Fatalf("Symbols: %v", err)
	}
	if report.SyntaxErrors || report.ErrorLine != 0 {
		t.Fatalf("report = %+v, want no syntax error and ErrorLine 0", report)
	}
}

func TestExtensionsOfGo(t *testing.T) {
	t.Parallel()
	lang, ok := ByName("go")
	if !ok {
		t.Fatal("go language missing")
	}
	if got := lang.Extensions(); !slices.Equal(got, []string{".go"}) {
		t.Fatalf("Extensions() = %v, want [.go]", got)
	}
}

func TestCompileErrorMatchesLibrary(t *testing.T) {
	t.Parallel()
	lang, ok := ByName("go")
	if !ok {
		t.Fatal("go language missing")
	}
	_, err := lang.Compile("(function_declaration")
	if err == nil {
		t.Fatal("Compile returned no error for an unclosed query")
	}
	if _, libErr := gotreesitter.NewQuery("(function_declaration", lang.l.grammar()); libErr == nil {
		t.Fatal("the library accepted an unclosed query")
	} else if want := "syntax: " + libErr.Error(); err.Error() != want {
		t.Fatalf("error = %q, want %q", err, want)
	}
}

func TestCaptureMatchIndexAdjacent(t *testing.T) {
	t.Parallel()
	lang, ok := ByName("go")
	if !ok {
		t.Fatal("go language missing")
	}
	query, err := lang.Compile("(type_spec name: (type_identifier) @name type: (_) @type)")
	if err != nil {
		t.Fatal(err)
	}
	src := []byte("package a\n\ntype A struct{}\n\ntype B int\n")
	captures, report, err := lang.NewParser().Captures(t.Context(), query, src)
	if err != nil {
		t.Fatal(err)
	}
	if report.SyntaxErrors {
		t.Fatal("valid source reported syntax errors")
	}
	want := []Capture{
		{Name: "name", Line: 3, Column: 6, StartByte: 16, EndByte: 17, Match: 0},
		{Name: "type", Line: 3, Column: 8, StartByte: 18, EndByte: 26, Match: 0},
		{Name: "name", Line: 5, Column: 6, StartByte: 33, EndByte: 34, Match: 1},
		{Name: "type", Line: 5, Column: 8, StartByte: 35, EndByte: 38, Match: 1},
	}
	if !slices.Equal(captures, want) {
		t.Fatalf("captures = %+v\nwant %+v", captures, want)
	}
}

func TestCapturesRejectsCancelledContext(t *testing.T) {
	t.Parallel()
	lang, ok := ByName("go")
	if !ok {
		t.Fatal("go language missing")
	}
	query, err := lang.Compile("(function_declaration name: (identifier) @name)")
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	_, _, err = lang.NewParser().Captures(ctx, query, []byte("package a\n"))
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("error = %v, want context.Canceled", err)
	}
}
