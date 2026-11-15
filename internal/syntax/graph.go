package syntax

import (
	"cmp"
	"context"
	"fmt"
	"go/types"
	"slices"

	"github.com/odvcencio/gotreesitter"
)

type RefKind string

const (
	RefCall RefKind = "call"
	RefType RefKind = "type"
)

type Ref struct {
	Kind   RefKind
	Name   string
	Line   int
	Column int
	Scope  int
}

type Import struct {
	Spec string
	Line int
}

type Graph struct {
	Symbols []Symbol
	Refs    []Ref
	Imports []Import
}

type refCandidate struct {
	kind   RefKind
	name   string
	start  int
	line   int
	column int
}

func (l *language) hasGraph() bool {
	return l.graph != ""
}

func (p *Parser) Graph(ctx context.Context, src []byte) (Graph, Report, error) {
	if !p.lang.hasGraph() {
		return Graph{}, Report{}, fmt.Errorf("syntax: no graph for %s", p.lang.name)
	}
	query, err := p.lang.graphQuery()
	if err != nil {
		return Graph{}, Report{}, err
	}
	tree, report, err := p.parse(ctx, src)
	if ctx.Err() != nil {
		tree.Release()
		return Graph{}, report, fmt.Errorf("syntax: %w", ctx.Err())
	}
	if err != nil {
		return Graph{}, Report{Stopped: true}, nil
	}
	if tree == nil {
		return Graph{}, report, nil
	}
	defer tree.Release()
	graph, report, err := p.graphOfTree(src, tree, query, report)
	if ctx.Err() != nil {
		return Graph{}, report, fmt.Errorf("syntax: %w", ctx.Err())
	}
	return graph, report, err
}

func (p *Parser) graphOfTree(src []byte, tree *gotreesitter.Tree, query *Query, report Report) (Graph, Report, error) {
	outline, err := p.outlineTree(tree)
	if err != nil {
		return Graph{}, report, err
	}
	if outline.stopped {
		return Graph{}, Report{Stopped: true}, nil
	}
	captures := capturesTree(query, tree)
	return Graph{
		Symbols: outline.publicSymbols(),
		Refs:    graphRefs(p.lang.name, src, outline, captures),
		Imports: graphImports(p.lang.name, src, captures),
	}, report, nil
}

func graphRefs(lang string, src []byte, outline treeOutline, captures []Capture) []Ref {
	var refs []Ref
	for _, c := range dedupeRefs(refCandidates(lang, src, outline.nameSpans, captures)) {
		refs = append(refs, Ref{Kind: c.kind, Name: c.name, Line: c.line, Column: c.column, Scope: innermostSymbol(outline.symbols, c.start)})
	}
	return refs
}

func refCandidates(lang string, src []byte, nameSpans [][2]int, captures []Capture) []refCandidate {
	names := newSpanSet(nameSpans)
	var found []refCandidate
	for _, capture := range captures {
		kind, ok := refKindOf(capture.Name)
		if !ok || names.covers(capture.StartByte) {
			continue
		}
		name := string(src[capture.StartByte:capture.EndByte])
		if kind == RefType && lang == "go" && predeclaredType(name) {
			continue
		}
		found = append(found, refCandidate{kind: kind, name: name, start: capture.StartByte, line: capture.Line, column: capture.Column})
	}
	return found
}

func refKindOf(capture string) (RefKind, bool) {
	kind := RefKind(capture)
	return kind, kind == RefCall || kind == RefType
}

func predeclaredType(name string) bool {
	_, ok := types.Universe.Lookup(name).(*types.TypeName)
	return ok
}

type spanSet [][2]int

func newSpanSet(spans [][2]int) spanSet {
	sorted := slices.Clone(spans)
	slices.SortFunc(sorted, func(a, b [2]int) int { return cmp.Compare(a[0], b[0]) })
	return sorted
}

func (s spanSet) covers(pos int) bool {
	after, _ := slices.BinarySearchFunc(s, pos+1, func(span [2]int, target int) int {
		return cmp.Compare(span[0], target)
	})
	return after > 0 && pos < s[after-1][1]
}

func dedupeRefs(found []refCandidate) []refCandidate {
	slices.SortFunc(found, func(a, b refCandidate) int {
		return cmp.Or(cmp.Compare(a.start, b.start), cmp.Compare(a.kind, b.kind), cmp.Compare(a.name, b.name))
	})
	kept := make([]refCandidate, 0, len(found))
	for _, c := range found {
		if !shadowed(kept, c) {
			kept = append(kept, c)
		}
	}
	return kept
}

func shadowed(kept []refCandidate, c refCandidate) bool {
	for i := len(kept) - 1; i >= 0 && kept[i].start == c.start; i-- {
		if kept[i].kind == RefCall && c.kind == RefType {
			return true
		}
		if kept[i].kind == c.kind && kept[i].name == c.name {
			return true
		}
	}
	return false
}

func innermostSymbol(symbols []outlinedSymbol, pos int) int {
	best := -1
	bestSize := 0
	for i, s := range symbols {
		if s.start > pos || pos >= s.end {
			continue
		}
		if size := s.end - s.start; best < 0 || size < bestSize {
			best = i
			bestSize = size
		}
	}
	return best
}

func graphImports(lang string, src []byte, captures []Capture) []Import {
	var out []Import
	for _, capture := range captures {
		if capture.Name != "import" {
			continue
		}
		for _, spec := range importSpecs(lang, string(src[capture.StartByte:capture.EndByte])) {
			out = append(out, Import{Spec: spec, Line: capture.Line})
		}
	}
	slices.SortFunc(out, func(a, b Import) int {
		return cmp.Or(cmp.Compare(a.Line, b.Line), cmp.Compare(a.Spec, b.Spec))
	})
	return slices.Compact(out)
}
