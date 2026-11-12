package syntax

import (
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
