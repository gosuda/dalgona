package syntax

import (
	"cmp"
	"context"
	"fmt"

	"github.com/odvcencio/gotreesitter"
)

type Kind string

const (
	kindFunction  Kind = "function"
	kindMethod    Kind = "method"
	kindClass     Kind = "class"
	kindInterface Kind = "interface"
	kindType      Kind = "type"
	kindEnum      Kind = "enum"
	kindConstant  Kind = "constant"
	kindVariable  Kind = "variable"
	kindModule    Kind = "module"
	kindMacro     Kind = "macro"
	kindSection   Kind = "section"
	kindOwner     Kind = "owner"
)

type Symbol struct {
	Kind       Kind
	Name       string
	Owner      string
	Start, End int
	Depth      int
}

func (s Symbol) Qualified() string {
	if s.Owner == "" {
		return s.Name
	}
	return s.Owner + "." + s.Name
}

type Report struct {
	SyntaxErrors bool
	Stopped      bool
	ErrorLine    int
}

var containerKinds = map[Kind]bool{
	kindClass:     true,
	kindInterface: true,
	kindType:      true,
	kindModule:    true,
	kindEnum:      true,
	kindSection:   true,
}

var kindByLibrary = map[string]Kind{
	"function":    kindFunction,
	"method":      kindMethod,
	"class":       kindClass,
	"interface":   kindInterface,
	"type":        kindType,
	"enum":        kindEnum,
	"constant":    kindConstant,
	"variable":    kindVariable,
	"module":      kindModule,
	"macro":       kindMacro,
	"section":     kindSection,
	"constructor": kindMethod,
	"record":      kindClass,
	"owner":       kindOwner,
}

func kindOf(library string) (Kind, error) {
	kind, ok := kindByLibrary[library]
	if !ok {
		return "", fmt.Errorf("syntax: unknown kind %s", library)
	}
	return kind, nil
}

type outlinedSymbol struct {
	symbol     Symbol
	start, end int
}

type treeOutline struct {
	symbols   []outlinedSymbol
	nameSpans [][2]int
	stopped   bool
}

func (o treeOutline) publicSymbols() []Symbol {
	var out []Symbol
	for _, s := range o.symbols {
		out = append(out, s.symbol)
	}
	return out
}

func (p *Parser) Symbols(ctx context.Context, src []byte) ([]Symbol, Report, error) {
	if p.lang.syntaxOnly() {
		return nil, Report{}, fmt.Errorf("syntax: no definition query for %s", p.lang.name)
	}
	tree, report, err := p.parse(ctx, src)
	if err != nil || tree == nil {
		return nil, report, err
	}
	defer tree.Release()
	outline, err := p.outlineTree(tree)
	if err != nil {
		return nil, report, err
	}
	if outline.stopped {
		return nil, Report{Stopped: true}, nil
	}
	return outline.publicSymbols(), report, nil
}

func (p *Parser) outlineTree(tree *gotreesitter.Tree) (treeOutline, error) {
	outliner, err := p.lang.outliner()
	if err != nil {
		return treeOutline{}, fmt.Errorf("syntax: outliner %s: %w", p.lang.name, err)
	}
	forest, report := outliner.OutlineTree(tree)
	if report.DeclineReason != "" {
		return treeOutline{}, fmt.Errorf("syntax: outline declined: %s", report.DeclineReason)
	}
	if report.Truncated {
		return treeOutline{stopped: true}, nil
	}
	symbols, err := flatten(forest)
	if err != nil {
		return treeOutline{}, err
	}
	return treeOutline{symbols: symbols, nameSpans: appendNameSpans(nil, forest)}, nil
}

func appendNameSpans(spans [][2]int, forest []gotreesitter.OutlineSymbol) [][2]int {
	for _, s := range forest {
		spans = append(spans, [2]int{int(s.NameRange.StartByte), int(s.NameRange.EndByte)})
		spans = appendNameSpans(spans, s.Children)
	}
	return spans
}

func flatten(forest []gotreesitter.OutlineSymbol) ([]outlinedSymbol, error) {
	var out []outlinedSymbol
	for _, root := range forest {
		if err := appendKept(&out, root, 0, ""); err != nil {
			return nil, err
		}
	}
	return out, nil
}

func appendKept(out *[]outlinedSymbol, s gotreesitter.OutlineSymbol, depth int, parent string) error {
	kind, err := kindOf(s.Kind)
	if err != nil {
		return err
	}
	owner := cmp.Or(s.Owner, parent)
	qualified := Symbol{Name: s.Name, Owner: owner}.Qualified()
	if kind == kindOwner {
		for _, child := range s.Children {
			if err := appendKept(out, child, depth, qualified); err != nil {
				return err
			}
		}
		return nil
	}
	*out = append(*out, outlinedSymbol{
		symbol: Symbol{
			Kind:  kind,
			Name:  s.Name,
			Owner: owner,
			Start: int(s.Range.StartPoint.Row) + 1,
			End:   lastLine(s.Range),
			Depth: depth,
		},
		start: int(s.Range.StartByte),
		end:   int(s.Range.EndByte),
	})
	if !containerKinds[kind] {
		return checkKinds(s.Children)
	}
	for _, child := range s.Children {
		if err := appendKept(out, child, depth+1, qualified); err != nil {
			return err
		}
	}
	return nil
}

func checkKinds(children []gotreesitter.OutlineSymbol) error {
	for _, child := range children {
		if _, err := kindOf(child.Kind); err != nil {
			return err
		}
		if err := checkKinds(child.Children); err != nil {
			return err
		}
	}
	return nil
}

func lastLine(r gotreesitter.Range) int {
	if r.EndPoint.Column == 0 && r.EndPoint.Row > r.StartPoint.Row {
		return int(r.EndPoint.Row)
	}
	return int(r.EndPoint.Row) + 1
}
