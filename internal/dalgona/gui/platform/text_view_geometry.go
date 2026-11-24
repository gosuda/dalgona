//go:build linux || windows

package platform

import (
	"image"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

func (v textView) rangeBoxes(r TextRange) []image.Rectangle {
	first, last := v.snap.textOrder[r.Start.Node], v.snap.textOrder[r.End.Node]
	if first > last || last >= len(v.parts()) {
		return nil
	}
	var boxes []image.Rectangle
	for _, id := range v.parts()[first : last+1] {
		n := v.snap.nodes[id]
		if !n.Bounds.Empty() {
			boxes = append(boxes, n.rangeBoxes(r.portion(n))...)
		}
	}
	return boxes
}

func (r TextRange) portion(n SemanticNode) inputtext.Range {
	span := inputtext.Range{End: len(n.Value)}
	if r.Start.Node == n.ID {
		span.Start = max(0, r.Start.offset(n))
	}
	if r.End.Node == n.ID {
		span.End = max(0, r.End.offset(n))
	}
	return span
}
