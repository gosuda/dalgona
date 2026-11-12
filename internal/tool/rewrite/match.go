package rewrite

import (
	"cmp"
	"fmt"
	"slices"

	"github.com/gosuda/dalgona/internal/syntax"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type span struct {
	start int
	end   int
}

type group struct {
	line  int
	spans map[string]span
}

type edit struct {
	start int
	end   int
	line  int
	text  []byte
}

func groupMatches(captures []syntax.Capture) ([]group, error) {
	var groups []group
	position := map[int]int{}
	for _, c := range captures {
		at, ok := position[c.Match]
		if !ok {
			at = len(groups)
			position[c.Match] = at
			groups = append(groups, group{line: c.Line, spans: map[string]span{}})
		}
		if _, dup := groups[at].spans[c.Name]; dup {
			return nil, fmt.Errorf("rewrite: @%s occurs twice in one match at line %d", c.Name, c.Line)
		}
		groups[at].spans[c.Name] = span{start: c.StartByte, end: c.EndByte}
	}
	return groups, nil
}

func editsOf(path string, src []byte, groups []group, target string, t template) ([]edit, error) {
	edits := make([]edit, 0, len(groups))
	for _, g := range groups {
		s, ok := g.spans[target]
		if !ok {
			return nil, fmt.Errorf("rewrite: target %s is not in the match at line %d", target, g.line)
		}
		if s.start == s.end {
			return nil, fmt.Errorf("rewrite: target %s has zero width at line %d", target, g.line)
		}
		text, err := expand(t, src, g)
		if err != nil {
			return nil, err
		}
		edits = append(edits, edit{start: s.start, end: s.end, line: g.line, text: text})
	}
	slices.SortFunc(edits, func(a, b edit) int {
		if c := cmp.Compare(a.start, b.start); c != 0 {
			return c
		}
		return cmp.Compare(a.end, b.end)
	})
	for i := 1; i < len(edits); i++ {
		if edits[i-1].end > edits[i].start {
			return nil, fmt.Errorf("rewrite: targets overlap in %s at line %d", path, edits[i].line)
		}
	}
	return edits, nil
}

func apply(src []byte, edits []edit) []byte {
	out := make([]byte, 0, len(src))
	pos := 0
	for _, e := range edits {
		out = append(out, src[pos:e.start]...)
		out = append(out, e.text...)
		pos = e.end
	}
	return append(out, src[pos:]...)
}

func splicesOf(edits []edit) []snapshot.Splice {
	out := make([]snapshot.Splice, 0, len(edits))
	for _, e := range edits {
		out = append(out, snapshot.Splice{Start: e.start, End: e.end, Inserted: len(e.text)})
	}
	return out
}
