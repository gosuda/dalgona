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
	csharp "github.com/odvcencio/gotreesitter/grammars/c_sharp"
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

type language struct {
	name       string
	extensions []string
	grammar    func() *gotreesitter.Language
	query      string
	outliner   func() (*gotreesitter.Outliner, error)
}

func newLanguage(name string, extensions []string, grammar func() *gotreesitter.Language, query string) *language {
	l := &language{name: name, extensions: extensions, grammar: grammar, query: query}
	l.outliner = sync.OnceValues(func() (*gotreesitter.Outliner, error) {
		return gotreesitter.NewOutliner(grammar(), query)
	})
	return l
}

func (l *language) syntaxOnly() bool {
	return l.query == ""
}

var registry = []*language{
	newLanguage("bash", []string{".sh"}, bash.Language, bashQuery),
	newLanguage("c", []string{".c", ".h"}, c.Language, cQuery),
	newLanguage("c_sharp", []string{".cs"}, csharp.Language, csharpQuery),
	newLanguage("cpp", []string{".cc", ".cpp", ".cxx", ".hpp", ".hh", ".hxx"}, cpp.Language, cppQuery),
	newLanguage("css", []string{".css"}, css.Language, ""),
	newLanguage("go", []string{".go"}, golang.Language, goQuery),
	newLanguage("html", []string{".html", ".htm"}, html.Language, ""),
	newLanguage("java", []string{".java"}, java.Language, javaQuery),
	newLanguage("javascript", []string{".js", ".jsx", ".mjs", ".cjs"}, javascript.Language, javascriptQuery),
	newLanguage("json", []string{".json"}, json.Language, ""),
	newLanguage("kotlin", []string{".kt", ".kts"}, kotlin.Language, kotlinQuery),
	newLanguage("lua", []string{".lua"}, lua.Language, luaQuery),
	newLanguage("markdown", []string{".md"}, markdown.Language, markdownQuery),
	newLanguage("php", []string{".php"}, php.Language, phpQuery),
	newLanguage("python", []string{".py"}, python.Language, pythonQuery),
	newLanguage("ruby", []string{".rb"}, ruby.Language, rubyQuery),
	newLanguage("rust", []string{".rs"}, rust.Language, rustQuery),
	newLanguage("svelte", []string{".svelte"}, svelte.Language, ""),
	newLanguage("swift", []string{".swift"}, swift.Language, swiftQuery),
	newLanguage("toml", []string{".toml"}, toml.Language, ""),
	newLanguage("tsx", []string{".tsx"}, tsx.Language, tsxQuery),
	newLanguage("typescript", []string{".ts", ".mts", ".cts"}, typescript.Language, typescriptQuery),
	newLanguage("yaml", []string{".yaml", ".yml"}, yaml.Language, ""),
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
	parser.SetTimeoutMicros(uint64(timeout / time.Microsecond)) //nolint:gosec // parseTimeout is a positive constant
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
