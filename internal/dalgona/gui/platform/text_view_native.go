//go:build linux || windows

package platform

import "github.com/gosuda/dalgona/internal/dal/textinput"

func (v textView) offset(p TextPoint, unit textinput.Unit) (int, error) {
	if _, err := v.part(p.Node); err != nil {
		return 0, err
	}
	at := 0
	for _, id := range v.parts()[:v.snap.textOrder[p.Node]+1] {
		n := v.snap.nodes[id]
		at += textinput.Count(n.TextBefore, unit)
		if id == p.Node && !p.End && p.Byte <= 0 {
			start := len(n.TextBefore) + p.Byte
			if start < 0 {
				return 0, textinput.InvalidOffset
			}
			return at - textinput.Count(n.TextBefore[start:], unit), nil
		}
		n, err := v.load(id)
		if err != nil {
			return 0, err
		}
		if id == p.Node {
			count, err := textinput.Convert(n.Value, p.offset(n), textinput.Bytes, unit)
			return at + count, err
		}
		at += textinput.Count(n.Value, unit)
	}
	return at, nil
}

func (v textView) selected() TextRange {
	var r TextRange
	for _, id := range v.parts() {
		n := v.snap.nodes[id]
		if n.Selection.Start == n.Selection.End {
			continue
		}
		if r.Start.Node == "" {
			r.Start = TextPoint{Node: id, Byte: n.Selection.Start}
		}
		r.End = TextPoint{Node: id, Byte: n.Selection.End}
	}
	return r
}
