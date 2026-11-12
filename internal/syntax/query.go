package syntax

import (
	"context"
	"fmt"
	"slices"

	"github.com/odvcencio/gotreesitter"
)

type Query struct {
	q    *gotreesitter.Query
	lang *language
}

func (l Language) Compile(pattern string) (*Query, error) {
	q, err := gotreesitter.NewQuery(pattern, l.l.grammar())
	if err != nil {
		return nil, fmt.Errorf("syntax: %w", err)
	}
	return &Query{q: q, lang: l.l}, nil
}

func (q *Query) CaptureNames() []string {
	names := q.q.CaptureNames()
	slices.Sort(names)
	return slices.Compact(names)
}

type Capture struct {
	Name      string
	Line      int
	Column    int
	StartByte int
	EndByte   int
	Match     int
}

func (p *Parser) Captures(ctx context.Context, q *Query, src []byte) ([]Capture, Report, error) {
	tree, report, err := p.parse(ctx, src)
	if err != nil || tree == nil {
		return nil, report, err
	}
	defer tree.Release()
	var out []Capture
	for index, match := range q.q.Execute(tree) {
		for _, item := range match.Captures {
			if item.Node == nil {
				continue
			}
			start := item.Node.StartPoint()
			out = append(out, Capture{
				Name:      item.Name,
				Line:      int(start.Row) + 1,
				Column:    int(start.Column) + 1,
				StartByte: int(item.Node.StartByte()),
				EndByte:   int(item.Node.EndByte()),
				Match:     index,
			})
		}
	}
	return out, report, nil
}
