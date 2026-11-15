package syntax

import (
	"cmp"
	"fmt"
	goast "go/ast"
	goparser "go/parser"
	"go/token"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	golang "github.com/odvcencio/gotreesitter/grammars/go"
	rust "github.com/odvcencio/gotreesitter/grammars/rust"
)

type goOwnerKey struct {
	name  string
	owner string
	start int
}

const goOwnerFixtureDir = "testdata/go/owners"

func TestSymbolsGoOwnersMatchGoParser(t *testing.T) {
	t.Parallel()
	files := goOwnerFiles(t)
	want := goParserMethodOwners(t, files)
	lang, ok := ByName("go")
	if !ok {
		t.Fatal("go language missing")
	}
	got := subjectMethodOwners(t, lang.NewParser(), files)
	if diff := goOwnerMismatch(want, got); diff != "" {
		t.Fatal(diff)
	}
	requireGoOwnerFixtureCoverage(t, want)
	requireGoOwnerComparisonRejectsWrongOracle(t, want, got)
	requireGoMethodOwnerEmptyWithoutOwnerRules(t)
}

func goOwnerMismatch(want, got []goOwnerKey) string {
	want = slices.Clone(want)
	got = slices.Clone(got)
	sortOwnerKeys(want)
	sortOwnerKeys(got)
	if slices.Equal(want, got) {
		return ""
	}
	return fmt.Sprintf("method owners from go/parser =\n%v\ngot\n%v", want, got)
}

func goOwnerFiles(t *testing.T) []string {
	t.Helper()
	files, err := filepath.Glob(filepath.Join(goOwnerFixtureDir, "*.go"))
	if err != nil {
		t.Fatal(err)
	}
	if len(files) < 5 {
		t.Fatalf("found %d owner fixtures under %s, want at least 5", len(files), goOwnerFixtureDir)
	}
	return files
}

var goOwnerFixtureCases = []struct{ label, owner, name string }{
	{"value receiver", "Counter", "Value"},
	{"pointer receiver", "Counter", "Add"},
	{"unnamed receiver", "Counter", "Kind"},
	{"unnamed pointer receiver", "Counter", "Reset"},
	{"blank receiver", "Counter", "Blank"},
	{"parenthesized receiver", "Counter", "Wrapped"},
	{"parenthesized pointer receiver", "Counter", "WrappedPointer"},
	{"receiver of a type declared after its methods", "Late", "Early"},
	{"generic receiver with one type parameter", "Box", "Get"},
	{"generic pointer receiver with one type parameter", "Box", "Set"},
	{"generic receiver with two type parameters", "Pair", "Key"},
	{"generic pointer receiver with two type parameters", "Pair", "Put"},
	{"parenthesized generic receiver", "Pair", "Both"},
	{"interface method", "Reader", "Read"},
	{"embedded interface with a method of its own", "ReadCloser", "Name"},
	{"interface with two methods", "Sizer", "Len"},
	{"constraint interface method", "Number", "String"},
	{"generic interface method", "Container", "Put"},
	{"method of a struct that embeds a struct", "Derived", "Name"},
	{"method of an embedded struct", "Base", "ID"},
	{"interface in a grouped declaration", "Store", "Save"},
	{"second interface in a grouped declaration", "Watcher", "Watch"},
	{"method of a struct in a grouped declaration", "Memory", "Load"},
	{"method of an array type in a grouped declaration", "Index", "First"},
	{"method of a function type in a grouped declaration", "Hook", "Run"},
	{"first interleaved type", "Left", "Push"},
	{"second interleaved type", "Right", "Push"},
}

func requireGoOwnerFixtureCoverage(t *testing.T, want []goOwnerKey) {
	t.Helper()
	for _, c := range goOwnerFixtureCases {
		if !slices.ContainsFunc(want, func(key goOwnerKey) bool { return key.owner == c.owner && key.name == c.name }) {
			t.Fatalf("fixtures under %s hold no %s %s.%s", goOwnerFixtureDir, c.label, c.owner, c.name)
		}
	}
}

func swapPushOwners(keys []goOwnerKey) []goOwnerKey {
	var pushes []int
	for i, key := range keys {
		if key.name == "Push" {
			pushes = append(pushes, i)
		}
	}
	a, b := pushes[0], pushes[1]
	keys[a].owner, keys[b].owner = keys[b].owner, keys[a].owner
	return keys
}

func requireGoOwnerComparisonRejectsWrongOracle(t *testing.T, want, got []goOwnerKey) {
	t.Helper()
	mutations := []struct {
		label  string
		mutate func([]goOwnerKey) []goOwnerKey
	}{
		{"a wrong owner", func(keys []goOwnerKey) []goOwnerKey { keys[0].owner += "X"; return keys }},
		{"a wrong name", func(keys []goOwnerKey) []goOwnerKey { keys[0].name += "X"; return keys }},
		{"a wrong line", func(keys []goOwnerKey) []goOwnerKey { keys[0].start++; return keys }},
		{"swapped owners of two methods with one name", swapPushOwners},
		{"a missing row", func(keys []goOwnerKey) []goOwnerKey { return keys[1:] }},
		{"an extra row", func(keys []goOwnerKey) []goOwnerKey { return append(keys, keys[0]) }},
	}
	for _, m := range mutations {
		if goOwnerMismatch(m.mutate(slices.Clone(want)), got) == "" {
			t.Fatalf("comparison accepted an oracle with %s", m.label)
		}
	}
}

func goParserMethodOwners(t *testing.T, files []string) []goOwnerKey {
	t.Helper()
	var want []goOwnerKey
	for _, path := range files {
		src, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		fset := token.NewFileSet()
		file, err := goparser.ParseFile(fset, path, src, 0)
		if err != nil {
			t.Fatalf("go/parser %s: %v", path, err)
		}
		for _, decl := range file.Decls {
			want = appendGoParserOwner(want, fset, decl)
		}
	}
	if len(want) < 10 {
		t.Fatalf("go/parser found %d owned methods, want at least 10", len(want))
	}
	return want
}

func appendGoParserOwner(want []goOwnerKey, fset *token.FileSet, decl goast.Decl) []goOwnerKey {
	switch decl := decl.(type) {
	case *goast.FuncDecl:
		if decl.Recv == nil || len(decl.Recv.List) == 0 {
			return want
		}
		return append(want, goOwnerKey{decl.Name.Name, receiverTypeName(decl.Recv.List[0].Type), fset.Position(decl.Pos()).Line})
	case *goast.GenDecl:
		if decl.Tok != token.TYPE {
			return want
		}
		for _, spec := range decl.Specs {
			want = appendInterfaceOwners(want, fset, spec)
		}
	}
	return want
}

func appendInterfaceOwners(want []goOwnerKey, fset *token.FileSet, spec goast.Spec) []goOwnerKey {
	ts, ok := spec.(*goast.TypeSpec)
	if !ok {
		return want
	}
	it, ok := ts.Type.(*goast.InterfaceType)
	if !ok {
		return want
	}
	for _, field := range it.Methods.List {
		if len(field.Names) == 0 {
			continue
		}
		want = append(want, goOwnerKey{field.Names[0].Name, ts.Name.Name, fset.Position(field.Names[0].Pos()).Line})
	}
	return want
}

func subjectMethodOwners(t *testing.T, parser *Parser, files []string) []goOwnerKey {
	t.Helper()
	var got []goOwnerKey
	for _, path := range files {
		src, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		symbols, report, err := parser.Symbols(t.Context(), src)
		if err != nil {
			t.Fatalf("Symbols %s: %v", path, err)
		}
		if report.Stopped {
			t.Fatalf("Symbols %s stopped", path)
		}
		for _, symbol := range symbols {
			if symbol.Kind == kindMethod {
				got = append(got, goOwnerKey{symbol.Name, symbol.Owner, symbol.Start})
			}
		}
	}
	return got
}

func requireGoMethodOwnerEmptyWithoutOwnerRules(t *testing.T) {
	t.Helper()
	ruleless := Language{l: newLanguage(languageSpec{name: "go", extensions: []string{".go"}, grammar: golang.Language, query: goQuery})}.NewParser()
	symbols, _, err := ruleless.Symbols(t.Context(), []byte("package p\n\ntype T struct{}\n\nfunc (t T) M() {}\n"))
	if err != nil {
		t.Fatalf("Symbols without owner rules: %v", err)
	}
	for _, symbol := range symbols {
		if symbol.Name == "M" && symbol.Owner != "" {
			t.Fatalf("method M owner = %q without owner rules, want empty", symbol.Owner)
		}
	}
}

func receiverTypeName(expr goast.Expr) string {
	switch expr := expr.(type) {
	case *goast.Ident:
		return expr.Name
	case *goast.StarExpr:
		return receiverTypeName(expr.X)
	case *goast.IndexExpr:
		return receiverTypeName(expr.X)
	case *goast.IndexListExpr:
		return receiverTypeName(expr.X)
	case *goast.ParenExpr:
		return receiverTypeName(expr.X)
	}
	return ""
}

func sortOwnerKeys(keys []goOwnerKey) {
	slices.SortFunc(keys, func(a, b goOwnerKey) int {
		return cmp.Or(cmp.Compare(a.name, b.name), cmp.Compare(a.owner, b.owner), cmp.Compare(a.start, b.start))
	})
}

type ownerWant struct {
	kind      Kind
	name      string
	owner     string
	qualified string
	start     int
	depth     int
}

func TestSymbolsOwnersGeneratedPrograms(t *testing.T) {
	t.Parallel()
	cases := []ownerCase{
		{"go", "package gen\n\ntype Service struct{}\n\nfunc (s *Service) Start() {}\n\nfunc standalone() {}\n", []ownerWant{
			{kindType, "Service", "", "Service", 3, 0},
			{kindMethod, "Start", "Service", "Service.Start", 5, 0},
			{kindFunction, "standalone", "", "standalone", 7, 0},
		}},
		{"typescript", "export class App {\n  run(): void {}\n}\nexport function alone() {}\n", []ownerWant{
			{kindClass, "App", "", "App", 1, 0},
			{kindMethod, "run", "App", "App.run", 2, 1},
			{kindFunction, "alone", "", "alone", 4, 0},
		}},
		{"python", "class Repo:\n    def find(self):\n        pass\n\ndef top():\n    pass\n", []ownerWant{
			{kindClass, "Repo", "", "Repo", 1, 0},
			{kindMethod, "find", "Repo", "Repo.find", 2, 1},
			{kindFunction, "top", "", "top", 5, 0},
		}},
		{"rust", "struct Ctx;\nimpl Ctx {\n    fn go(&self) {}\n}\nfn free() {}\n", []ownerWant{
			{kindType, "Ctx", "", "Ctx", 1, 0},
			{kindMethod, "go", "Ctx", "Ctx.go", 3, 0},
			{kindFunction, "free", "", "free", 5, 0},
		}},
		{"java", "public class Engine {\n    void drive() {}\n}\n", []ownerWant{
			{kindClass, "Engine", "", "Engine", 1, 0},
			{kindMethod, "drive", "Engine", "Engine.drive", 2, 1},
		}},
	}
	for _, tc := range cases {
		t.Run(tc.lang, func(t *testing.T) {
			runGeneratedOwnersCase(t, tc)
		})
	}
	requireRustMethodOwnerEmptyWithoutOwnerRows(t)
}

type ownerCase struct {
	lang string
	src  string
	want []ownerWant
}

func runGeneratedOwnersCase(t *testing.T, tc ownerCase) {
	t.Helper()
	lang, ok := ByName(tc.lang)
	if !ok {
		t.Fatalf("%s language missing", tc.lang)
	}
	symbols, report, err := lang.NewParser().Symbols(t.Context(), []byte(tc.src))
	if err != nil {
		t.Fatalf("Symbols: %v", err)
	}
	if report.Stopped || report.SyntaxErrors {
		t.Fatalf("report = %+v, want clean parse", report)
	}
	requireOwnerWants(t, symbols, tc.want)
}

func requireOwnerWants(t *testing.T, symbols []Symbol, want []ownerWant) {
	t.Helper()
	if len(symbols) != len(want) {
		t.Fatalf("symbols =\n%v\nwant %d rows", symbols, len(want))
	}
	for _, wantRow := range want {
		requireOwnerWant(t, symbols, wantRow)
	}
}

func requireOwnerWant(t *testing.T, symbols []Symbol, wantRow ownerWant) {
	t.Helper()
	found := false
	for _, symbol := range symbols {
		if symbol.Name != wantRow.name {
			continue
		}
		found = true
		if symbol.Kind != wantRow.kind || symbol.Owner != wantRow.owner || symbol.Start != wantRow.start || symbol.Depth != wantRow.depth {
			t.Fatalf("symbol %+v, want %+v", symbol, wantRow)
		}
		if symbol.Qualified() != wantRow.qualified {
			t.Fatalf("Qualified(%s) = %q, want %q", symbol.Name, symbol.Qualified(), wantRow.qualified)
		}
	}
	if !found {
		t.Fatalf("symbol %s missing from %v", wantRow.name, symbols)
	}
}

func requireRustMethodOwnerEmptyWithoutOwnerRows(t *testing.T) {
	t.Helper()
	var stripped []string
	for line := range strings.SplitSeq(rustQuery, "\n") {
		if strings.Contains(line, "@definition.owner") {
			continue
		}
		stripped = append(stripped, line)
	}
	ruleless := Language{l: newLanguage(languageSpec{name: "rust", extensions: []string{".rs"}, grammar: rust.Language, query: strings.Join(stripped, "\n")})}.NewParser()
	symbols, _, err := ruleless.Symbols(t.Context(), []byte("struct Ctx;\nimpl Ctx {\n    fn go(&self) {}\n}\n"))
	if err != nil {
		t.Fatalf("Symbols without owner rows: %v", err)
	}
	for _, symbol := range symbols {
		if symbol.Name == "go" && symbol.Owner != "" {
			t.Fatalf("method go owner = %q without owner rows, want empty", symbol.Owner)
		}
	}
}

func TestSymbolsRustImplOwners(t *testing.T) {
	t.Parallel()
	src := `struct Plain;
struct Wrapper<T> { inner: T }
mod scope { pub struct Inner; pub struct Mux<T> { inner: T } }
impl Plain { fn plain(&self) {} }
impl Wrapper<u8> { fn generic(&self) {} }
impl scope::Inner { fn scoped(&self) {} }
impl scope::Mux<u8> { fn generic_scoped(&self) {} }
pub trait Tool { fn tool(&self); }
impl Tool for Plain { fn tool(&self) {} }
`
	want := []struct {
		name  string
		owner string
		depth int
	}{
		{"plain", "Plain", 0},
		{"generic", "Wrapper", 0},
		{"scoped", "Inner", 0},
		{"generic_scoped", "Mux", 0},
		{"tool", "Tool", 1},
		{"tool", "Plain", 0},
	}
	lang, ok := ByName("rust")
	if !ok {
		t.Fatal("rust language missing")
	}
	symbols, report, err := lang.NewParser().Symbols(t.Context(), []byte(src))
	if err != nil {
		t.Fatalf("Symbols: %v", err)
	}
	if report.Stopped || report.SyntaxErrors {
		t.Fatalf("report = %+v, want clean parse", report)
	}
	var got []struct {
		name  string
		owner string
		depth int
	}
	for _, symbol := range symbols {
		if symbol.Kind == kindOwner {
			t.Fatalf("owner-kind symbol leaked a row: %+v", symbol)
		}
		if symbol.Kind == kindMethod {
			got = append(got, struct {
				name  string
				owner string
				depth int
			}{symbol.Name, symbol.Owner, symbol.Depth})
		}
	}
	if !slices.Equal(got, want) {
		t.Fatalf("method rows = %v, want %v", got, want)
	}
}

func TestSymbolsMarkdownHeadingOwners(t *testing.T) {
	t.Parallel()
	lang, ok := ByName("markdown")
	if !ok {
		t.Fatal("markdown language missing")
	}
	symbols, report, err := lang.NewParser().Symbols(t.Context(), []byte("# Title\n\n## Setup\n"))
	if err != nil {
		t.Fatalf("Symbols: %v", err)
	}
	if report.Stopped || report.SyntaxErrors {
		t.Fatalf("report = %+v, want clean parse", report)
	}
	want := []Symbol{
		{Kind: "section", Name: "Title", Start: 1, End: 3, Depth: 0},
		{Kind: "section", Name: "Setup", Owner: "Title", Start: 3, End: 3, Depth: 1},
	}
	if !slices.Equal(symbols, want) {
		t.Fatalf("symbols =\n%v\nwant\n%v", symbols, want)
	}
}
