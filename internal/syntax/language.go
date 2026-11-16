package syntax

import (
	"context"
	_ "embed"
	"errors"
	"fmt"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/odvcencio/gotreesitter"
	bash "github.com/odvcencio/gotreesitter/grammars/bash"
	c "github.com/odvcencio/gotreesitter/grammars/c"
	"github.com/odvcencio/gotreesitter/grammars/c_sharp"
	cpp "github.com/odvcencio/gotreesitter/grammars/cpp"
	css "github.com/odvcencio/gotreesitter/grammars/css"
	golang "github.com/odvcencio/gotreesitter/grammars/go"
	html "github.com/odvcencio/gotreesitter/grammars/html"
	java "github.com/odvcencio/gotreesitter/grammars/java"
	javascript "github.com/odvcencio/gotreesitter/grammars/javascript"
	json "github.com/odvcencio/gotreesitter/grammars/json"
	kotlin "github.com/odvcencio/gotreesitter/grammars/kotlin"
	lua "github.com/odvcencio/gotreesitter/grammars/lua"
	markdown "github.com/odvcencio/gotreesitter/grammars/markdown"
	php "github.com/odvcencio/gotreesitter/grammars/php"
	python "github.com/odvcencio/gotreesitter/grammars/python"
	ruby "github.com/odvcencio/gotreesitter/grammars/ruby"
	grammarruntime "github.com/odvcencio/gotreesitter/grammars/runtime"
	rust "github.com/odvcencio/gotreesitter/grammars/rust"
	svelte "github.com/odvcencio/gotreesitter/grammars/svelte"
	swift "github.com/odvcencio/gotreesitter/grammars/swift"
	toml "github.com/odvcencio/gotreesitter/grammars/toml"
	tsx "github.com/odvcencio/gotreesitter/grammars/tsx"
	typescript "github.com/odvcencio/gotreesitter/grammars/typescript"
	yaml "github.com/odvcencio/gotreesitter/grammars/yaml"
)

const (
	parseTimeout           = 2 * time.Second
	parseMemoryBudgetBytes = 256 << 20
	parseIterationLimit    = 200_000
	parseStackDepthLimit   = 8_192
	parseNodeLimit         = 1_000_000
)

//go:embed queries/go.scm
var goQuery string

//go:embed queries/typescript.scm
var typescriptQuery string

//go:embed queries/tsx.scm
var tsxQuery string

//go:embed queries/javascript.scm
var javascriptQuery string

//go:embed queries/python.scm
var pythonQuery string

//go:embed queries/rust.scm
var rustQuery string

//go:embed queries/c.scm
var cQuery string

//go:embed queries/cpp.scm
var cppQuery string

//go:embed queries/c_sharp.scm
var csharpQuery string

//go:embed queries/java.scm
var javaQuery string

//go:embed queries/kotlin.scm
var kotlinQuery string

//go:embed queries/swift.scm
var swiftQuery string

//go:embed queries/ruby.scm
var rubyQuery string

//go:embed queries/php.scm
var phpQuery string

//go:embed queries/bash.scm
var bashQuery string

//go:embed queries/lua.scm
var luaQuery string

//go:embed queries/markdown.scm
var markdownQuery string

//go:embed queries/go.graph.scm
var goGraph string

//go:embed queries/typescript.graph.scm
var typescriptGraph string

//go:embed queries/python.graph.scm
var pythonGraph string

//go:embed queries/rust.graph.scm
var rustGraph string

//go:embed queries/java.graph.scm
var javaGraph string

type language struct {
	name       string
	extensions []string
	grammar    func() *gotreesitter.Language
	query      string
	graph      string
	outliner   func() (*gotreesitter.Outliner, error)
	graphQuery func() (*Query, error)
}

type languageSpec struct {
	name       string
	extensions []string
	grammar    func() *gotreesitter.Language
	query      string
	graph      string
	ownerRules []gotreesitter.OutlineOwnerRule
}

var goOwnerRules = []gotreesitter.OutlineOwnerRule{{
	NodeType:   "method_declaration",
	OwnerField: "receiver",
	Unwrap:     []string{"parameter_list", "parameter_declaration", "pointer_type", "generic_type", "parenthesized_type"},
	NameTypes:  []string{"type_identifier"},
}}

func newLanguage(spec languageSpec) *language {
	l := &language{name: spec.name, extensions: spec.extensions, grammar: spec.grammar, query: spec.query, graph: spec.graph}
	l.outliner = sync.OnceValues(func() (*gotreesitter.Outliner, error) {
		return gotreesitter.NewOutliner(spec.grammar(), spec.query, gotreesitter.WithOutlineOwnerRules(spec.ownerRules))
	})
	l.graphQuery = sync.OnceValues(func() (*Query, error) {
		return Language{l: l}.Compile(spec.graph)
	})
	return l
}

func (l *language) syntaxOnly() bool {
	return l.query == ""
}

var registry = []*language{
	newLanguage(languageSpec{name: "bash", extensions: []string{".sh"}, grammar: bash.Language, query: bashQuery}),
	newLanguage(languageSpec{name: "c", extensions: []string{".c", ".h"}, grammar: c.Language, query: cQuery}),
	newLanguage(languageSpec{name: "c_sharp", extensions: []string{".cs"}, grammar: c_sharp.Language, query: csharpQuery}),
	newLanguage(languageSpec{name: "cpp", extensions: []string{".cc", ".cpp", ".cxx", ".hpp", ".hh", ".hxx"}, grammar: cpp.Language, query: cppQuery}),
	newLanguage(languageSpec{name: "css", extensions: []string{".css"}, grammar: css.Language}),
	newLanguage(languageSpec{name: "go", extensions: []string{".go"}, grammar: golang.Language, query: goQuery, graph: goGraph, ownerRules: goOwnerRules}),
	newLanguage(languageSpec{name: "html", extensions: []string{".html", ".htm"}, grammar: html.Language}),
	newLanguage(languageSpec{name: "java", extensions: []string{".java"}, grammar: java.Language, query: javaQuery, graph: javaGraph}),
	newLanguage(languageSpec{name: "javascript", extensions: []string{".js", ".jsx", ".mjs", ".cjs"}, grammar: javascript.Language, query: javascriptQuery}),
	newLanguage(languageSpec{name: "json", extensions: []string{".json"}, grammar: json.Language}),
	newLanguage(languageSpec{name: "kotlin", extensions: []string{".kt", ".kts"}, grammar: kotlin.Language, query: kotlinQuery}),
	newLanguage(languageSpec{name: "lua", extensions: []string{".lua"}, grammar: lua.Language, query: luaQuery}),
	newLanguage(languageSpec{name: "markdown", extensions: []string{".md"}, grammar: markdown.Language, query: markdownQuery}),
	newLanguage(languageSpec{name: "php", extensions: []string{".php"}, grammar: php.Language, query: phpQuery}),
	newLanguage(languageSpec{name: "python", extensions: []string{".py"}, grammar: python.Language, query: pythonQuery, graph: pythonGraph}),
	newLanguage(languageSpec{name: "ruby", extensions: []string{".rb"}, grammar: ruby.Language, query: rubyQuery}),
	newLanguage(languageSpec{name: "rust", extensions: []string{".rs"}, grammar: rust.Language, query: rustQuery, graph: rustGraph}),
	newLanguage(languageSpec{name: "svelte", extensions: []string{".svelte"}, grammar: svelte.Language}),
	newLanguage(languageSpec{name: "swift", extensions: []string{".swift"}, grammar: swift.Language, query: swiftQuery}),
	newLanguage(languageSpec{name: "toml", extensions: []string{".toml"}, grammar: toml.Language}),
	newLanguage(languageSpec{name: "tsx", extensions: []string{".tsx"}, grammar: tsx.Language, query: tsxQuery}),
	newLanguage(languageSpec{name: "typescript", extensions: []string{".ts", ".mts", ".cts"}, grammar: typescript.Language, query: typescriptQuery, graph: typescriptGraph}),
	newLanguage(languageSpec{name: "yaml", extensions: []string{".yaml", ".yml"}, grammar: yaml.Language}),
}

type Language struct {
	l *language
}

func ForPath(path string) (Language, bool) {
	ext := strings.ToLower(filepath.Ext(path))
	for _, l := range registry {
		if slices.Contains(l.extensions, ext) {
			return Language{l: l}, true
		}
	}
	return Language{}, false
}

func ByName(name string) (Language, bool) {
	for _, l := range registry {
		if l.name == name {
			return Language{l: l}, true
		}
	}
	return Language{}, false
}

func Names() []string {
	names := make([]string, 0, len(registry))
	for _, l := range registry {
		names = append(names, l.name)
	}
	slices.Sort(names)
	return names
}

func (l Language) Name() string {
	return l.l.name
}

func (l Language) HasDefinitions() bool {
	return !l.l.syntaxOnly()
}

func (l Language) HasGraph() bool {
	return l.l.graph != ""
}

func (l Language) Extensions() []string {
	return slices.Clone(l.l.extensions)
}

func (l Language) NewParser() *Parser {
	return newParser(l.l, parseTimeout)
}

type Parser struct {
	lang    *language
	grammar *gotreesitter.Language
	parser  *gotreesitter.Parser
	timeout time.Duration
}

func newParser(lang *language, timeout time.Duration) *Parser {
	grammar := lang.grammar()
	parser := gotreesitter.NewParser(grammar)
	micros := max(0, timeout) / time.Microsecond
	parser.SetTimeoutMicros(uint64(micros))
	parser.SetMemoryBudgetBytes(parseMemoryBudgetBytes)
	parser.SetParseWorkLimits(gotreesitter.ParseWorkLimits{
		IterationLimit:  parseIterationLimit,
		StackDepthLimit: parseStackDepthLimit,
		NodeLimit:       parseNodeLimit,
	})
	return &Parser{lang: lang, grammar: grammar, parser: parser, timeout: timeout}
}

func tokenSourceFactory(name string, lang *gotreesitter.Language) gotreesitter.TokenSourceFactory {
	factory := grammarruntime.TokenSourceFactory(name)
	if factory == nil {
		return nil
	}
	return func(src []byte) (gotreesitter.TokenSource, error) {
		return factory(src, lang), nil
	}
}

func (p *Parser) parse(ctx context.Context, src []byte) (*gotreesitter.Tree, Report, error) {
	if err := ctx.Err(); err != nil {
		return nil, Report{}, fmt.Errorf("syntax: %w", err)
	}
	began := time.Now()
	var tree *gotreesitter.Tree
	var err error
	if factory := tokenSourceFactory(p.lang.name, p.grammar); factory != nil {
		tree, err = p.parser.ParseWithTokenSourceFactoryStrict(src, factory)
	} else {
		tree, err = p.parser.ParseStrict(src)
	}
	stopped := errors.Is(err, gotreesitter.ErrParseStoppedEarly) || time.Since(began) >= p.timeout
	if err != nil || stopped {
		tree.Release()
		if stopped {
			return nil, Report{Stopped: true}, nil
		}
		return nil, Report{}, fmt.Errorf("syntax: parse %s: %w", p.lang.name, err)
	}
	root := tree.RootNode()
	return tree, Report{SyntaxErrors: root.HasError(), ErrorLine: firstErrorLine(root)}, nil
}

func firstErrorLine(n *gotreesitter.Node) int {
	if !n.HasError() {
		return 0
	}
	if n.IsError() || n.IsMissing() {
		return int(n.StartPoint().Row) + 1
	}
	for i := range n.ChildCount() {
		if line := firstErrorLine(n.Child(i)); line > 0 {
			return line
		}
	}
	return 0
}
