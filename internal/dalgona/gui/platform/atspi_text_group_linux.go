//go:build linux

package platform

import (
	"image"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

func (v textView) atspiPart(offset int) (SemanticNode, int, error) {
	p, err := v.point(offset, inputtext.Scalars)
	if err != nil {
		return SemanticNode{}, 0, err
	}
	parts := v.parts()[v.snap.textOrder[p.Node]:]
	for i, id := range parts {
		part := v.snap.nodes[id]
		if id != p.Node {
			p = TextPoint{Node: id, Byte: -len(part.TextBefore)}
		}
		if p.Byte < 0 {
			return SemanticNode{Value: part.TextBefore}, len(part.TextBefore) + p.Byte, nil
		}
		n, err := v.load(id)
		if err != nil || p.offset(n) < len(n.Value) || i+1 == len(parts) {
			return n, p.offset(n), err
		}
	}
	return SemanticNode{}, 0, inputtext.InvalidOffset
}

func atspiGroupSegment(o *atspiObject, args []any) ([]any, error) {
	offset := max(int(args[0].(int32)), 0)
	n, at, err := o.textView().atspiPart(offset)
	if err != nil {
		return nil, atspiTextError(err)
	}
	start, end, err := atspiSegment(n, at, args[1].(uint32))
	if err != nil {
		return nil, err
	}
	end = objectEnd(n.Value[:end], start)
	base := offset - inputtext.Count(n.Value[:at], inputtext.Scalars)
	first := clampInt32(base + inputtext.Count(n.Value[:start], inputtext.Scalars))
	last := clampInt32(base + inputtext.Count(n.Value[:end], inputtext.Scalars))
	return []any{n.Value[start:end], first, last}, nil
}

func atspiGroupCharacter(o *atspiObject, offset int) ([]any, error) {
	n, at, err := o.textView().atspiPart(offset)
	if err != nil {
		return nil, atspiTextError(err)
	}
	if at >= len(n.Value) {
		return nil, atspiInvalid("the offset is at the end of the text")
	}
	r, _ := utf8.DecodeRuneInString(n.Value[at:])
	return []any{r}, nil
}

func atspiGroupCharacterBox(o *atspiObject, args []any) ([]any, error) {
	n, at, err := o.textView().atspiPart(int(args[0].(int32)))
	if err != nil {
		return nil, atspiTextError(err)
	}
	box, ok := n.characterBox(at)
	return o.textBox(box, ok, args[1].(uint32))
}

func atspiGroupRangeBox(o *atspiObject, args []any) ([]any, error) {
	first, last := int(args[0].(int32)), int(args[1].(int32))
	view := o.textView()
	r, err := view.unitsRange(min(first, last), max(first, last), inputtext.Scalars)
	if err != nil {
		return nil, atspiTextError(err)
	}
	var box image.Rectangle
	for _, b := range view.rangeBoxes(r) {
		box = box.Union(b)
	}
	return o.textBox(box, !box.Empty(), args[2].(uint32))
}

func atspiGroupOffsetAtPoint(o *atspiObject, point image.Point) ([]any, error) {
	v := o.textView()
	for _, id := range v.parts() {
		n := v.snap.nodes[id]
		line, ok := n.closestLine(point)
		if !ok || !point.In(line.Bounds) {
			continue
		}
		at := line.nearest(n.Value, point.X)
		offset, err := v.offset(TextPoint{Node: id, Byte: at}, inputtext.Scalars)
		return []any{clampInt32(offset)}, atspiTextError(err)
	}
	return []any{int32(-1)}, nil
}
