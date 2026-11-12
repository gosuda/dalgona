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
	golang "github.com/odvcencio/gotreesitter/grammars/go"
	grammarruntime "github.com/odvcencio/gotreesitter/grammars/runtime"
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
	newLanguage("go", []string{".go"}, golang.Language, goQuery),
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
	return tree, Report{SyntaxErrors: tree.RootNode().HasError()}, nil
}
