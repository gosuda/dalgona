package syntax

import (
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
)

type Symbol struct {
	Kind       Kind
	Name       string
	Start, End int
	Depth      int
}

type Report struct {
	SyntaxErrors bool
	Stopped      bool
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
}

func kindOf(library string) (Kind, error) {
	kind, ok := kindByLibrary[library]
	if !ok {
		return "", fmt.Errorf("syntax: unknown kind %s", library)
	}
	return kind, nil
}

func (p *Parser) Symbols(ctx context.Context, src []byte) ([]Symbol, Report, error) {
	if p.lang.syntaxOnly() {
		return nil, Report{}, fmt.Errorf("syntax: no definition query for %s", p.lang.name)
	}
	outliner, err := p.lang.outliner()
	if err != nil {
		return nil, Report{}, fmt.Errorf("syntax: outliner %s: %w", p.lang.name, err)
	}
	tree, report, err := p.parse(ctx, src)
	if err != nil || tree == nil {
		return nil, report, err
	}
	defer tree.Release()
	forest, outline := outliner.OutlineTree(tree)
	if outline.DeclineReason != "" {
		return nil, report, fmt.Errorf("syntax: outline declined: %s", outline.DeclineReason)
	}
	if outline.Truncated {
		return nil, Report{Stopped: true}, nil
	}
	symbols, err := flatten(forest)
	if err != nil {
		return nil, report, err
	}
	return symbols, report, nil
}

func flatten(forest []gotreesitter.OutlineSymbol) ([]Symbol, error) {
	var out []Symbol
	for _, s := range forest {
		if err := appendKept(&out, s, 0); err != nil {
			return nil, err
		}
	}
	return out, nil
}

func appendKept(out *[]Symbol, s gotreesitter.OutlineSymbol, depth int) error {
	kind, err := kindOf(s.Kind)
	if err != nil {
		return err
	}
	*out = append(*out, Symbol{
		Kind:  kind,
		Name:  s.Name,
		Start: int(s.Range.StartPoint.Row) + 1,
		End:   int(s.Range.EndPoint.Row) + 1,
		Depth: depth,
	})
	if !containerKinds[kind] {
		return checkKinds(s.Children)
	}
	for _, child := range s.Children {
		if err := appendKept(out, child, depth+1); err != nil {
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
