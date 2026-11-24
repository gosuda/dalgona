package platform

import (
	"image"
	"unicode"
	"unicode/utf16"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

const (
	uiaUnitCharacter int32 = 0
	uiaUnitFormat    int32 = 1
	uiaUnitWord      int32 = 2
	uiaUnitLine      int32 = 3
	uiaUnitParagraph int32 = 4
	uiaUnitPage      int32 = 5
	uiaUnitDocument  int32 = 6
)

const (
	uiaEndpointStart int32 = 0
	uiaEndpointEnd   int32 = 1
)

type uiaTextUnit uint8

const (
	uiaByCharacter uiaTextUnit = iota + 1
	uiaByWord
	uiaByLine
	uiaByParagraph
	uiaByDocument
)

var uiaTextUnits = map[int32]uiaTextUnit{
	uiaUnitCharacter: uiaByCharacter,
	uiaUnitFormat:    uiaByDocument,
	uiaUnitWord:      uiaByWord,
	uiaUnitLine:      uiaByLine,
	uiaUnitParagraph: uiaByParagraph,
	uiaUnitPage:      uiaByDocument,
	uiaUnitDocument:  uiaByDocument,
}

func (u uiaTextUnit) starts(text string, i int) bool {
	if i == 0 {
		return true
	}
	switch u {
	case uiaByCharacter:
		return true
	case uiaByWord:
		return uiaWordFloor(text, i) == i
	case uiaByLine:
		return text[i-1] == '\n'
	case uiaByParagraph:
		return text[i-1] == '\n' && text[i] != '\n'
	case uiaByDocument:
	}
	return false
}

func (u uiaTextUnit) next(text string, i int) int {
	switch u {
	case uiaByDocument:
		return len(text)
	case uiaByWord:
		return uiaWordNext(text, i)
	case uiaByCharacter, uiaByLine, uiaByParagraph:
	}
	for i < len(text) {
		step, err := inputtext.NextBoundary(text, i)
		if err != nil || step == i {
			return len(text)
		}
		i = step
		if i < len(text) && u.starts(text, i) {
			return i
		}
	}
	return len(text)
}

func (u uiaTextUnit) previous(text string, i int) int {
	switch u {
	case uiaByDocument:
		return 0
	case uiaByWord:
		return uiaWordPrevious(text, i)
	case uiaByCharacter, uiaByLine, uiaByParagraph:
	}
	for i > 0 {
		step, err := inputtext.PreviousBoundary(text, i)
		if err != nil || step == i {
			return 0
		}
		i = step
		if u.starts(text, i) {
			return i
		}
	}
	return 0
}

func (u uiaTextUnit) floor(text string, i int) int {
	if i < len(text) && u.starts(text, i) {
		return i
	}
	return u.previous(text, i)
}

func uiaWordFloor(text string, i int) int {
	start, err := previousWordStart(text, i, visibleWord)
	if err != nil {
		return 0
	}
	return uiaSnap(text, start)
}

func uiaWordNext(text string, i int) int {
	start, err := nextWordStart(text, i, visibleWord)
	if err != nil {
		return len(text)
	}
	paint, err := inputtext.PaintRange(text, inputtext.Range{Start: start, End: start})
	if err != nil {
		return len(text)
	}
	return paint.End
}

func uiaWordPrevious(text string, i int) int {
	before, err := inputtext.PreviousBoundary(text, i)
	if err != nil || i == 0 {
		return 0
	}
	return uiaWordFloor(text, before)
}

func uiaSnap(text string, i int) int {
	i = min(max(i, 0), len(text))
	for i < len(text) && !utf8.RuneStart(text[i]) {
		i--
	}
	paint, err := inputtext.PaintRange(text, inputtext.Range{Start: i, End: i})
	if err != nil || i == len(text) {
		return i
	}
	return paint.Start
}

func uiaSnapRange(text string, r inputtext.Range) inputtext.Range {
	start, end := uiaSnap(text, r.Start), uiaSnap(text, r.End)
	return inputtext.Range{Start: min(start, end), End: max(start, end)}
}

func uiaExpand(text string, r inputtext.Range, u uiaTextUnit) inputtext.Range {
	start := u.floor(text, r.Start)
	return inputtext.Range{Start: start, End: u.next(text, start)}
}

func uiaMoveOffset(text string, at int, u uiaTextUnit, count int32) (int, int32) {
	var moved int32
	for ; count > 0 && at < len(text); count-- {
		at = u.next(text, at)
		moved++
	}
	for ; count < 0 && at > 0; count++ {
		at = u.previous(text, at)
		moved--
	}
	return at, moved
}

func uiaValidEndpoint(endpoint int32) bool {
	return endpoint == uiaEndpointStart || endpoint == uiaEndpointEnd
}

func uiaEndpointOf(r inputtext.Range, endpoint int32) int {
	if endpoint == uiaEndpointEnd {
		return r.End
	}
	return r.Start
}

func uiaWithEndpoint(r inputtext.Range, endpoint int32, at int) inputtext.Range {
	if endpoint == uiaEndpointEnd {
		return inputtext.Range{Start: min(r.Start, at), End: at}
	}
	return inputtext.Range{Start: at, End: max(at, r.End)}
}

func uiaMoveEndpoint(text string, r inputtext.Range, endpoint int32, u uiaTextUnit, count int32) (inputtext.Range, int32) {
	at, moved := uiaMoveOffset(text, uiaEndpointOf(r, endpoint), u, count)
	return uiaWithEndpoint(r, endpoint, at), moved
}

func uiaMove(text string, r inputtext.Range, u uiaTextUnit, count int32) (inputtext.Range, int32) {
	if r.Start == r.End {
		at, moved := uiaMoveOffset(text, r.Start, u, count)
		return inputtext.Range{Start: at, End: at}, moved
	}
	at, moved := u.floor(text, r.Start), int32(0)
	for ; count > 0; count-- {
		step := u.next(text, at)
		if step >= len(text) {
			break
		}
		at, moved = step, moved+1
	}
	for ; count < 0 && at > 0; count++ {
		at, moved = u.previous(text, at), moved-1
	}
	if moved == 0 {
		return r, 0
	}
	return inputtext.Range{Start: at, End: u.next(text, at)}, moved
}

func uiaRangeText(text string, r inputtext.Range, maxUnits int) string {
	if r.End-r.Start > TextObjectLimit {
		r.End = objectEnd(text, r.Start)
	}
	s := text[r.Start:r.End]
	if maxUnits < 0 {
		return s
	}
	prefix, _ := utf16Prefix(s, maxUnits)
	return prefix
}

func utf16Prefix(s string, units int) (string, int) {
	for i, c := range s {
		units -= utf16.RuneLen(c)
		if units < 0 {
			return s[:i], 0
		}
	}
	return s, units
}

func uiaFind(text string, r inputtext.Range, needle string, backward, fold bool) (inputtext.Range, bool) {
	if needle == "" {
		return inputtext.Range{}, false
	}
	starts := make([]int, 0, r.End-r.Start)
	for i := r.Start; i < r.End; i++ {
		if utf8.RuneStart(text[i]) {
			starts = append(starts, i)
		}
	}
	for k := range starts {
		at := starts[k]
		if backward {
			at = starts[len(starts)-1-k]
		}
		end, ok := uiaMatchAt(text[:r.End], at, needle, fold)
		if ok && inputtext.CheckRange(text, inputtext.Range{Start: at, End: end}) == nil {
			return inputtext.Range{Start: at, End: end}, true
		}
	}
	return inputtext.Range{}, false
}

func uiaMatchAt(text string, at int, needle string, fold bool) (int, bool) {
	for _, want := range needle {
		got, size := utf8.DecodeRuneInString(text[at:])
		if size == 0 || got != want && (!fold || !uiaFoldEqual(got, want)) {
			return 0, false
		}
		at += size
	}
	return at, true
}

func uiaFoldEqual(a, b rune) bool {
	for r := unicode.SimpleFold(a); r != a; r = unicode.SimpleFold(r) {
		if r == b {
			return true
		}
	}
	return false
}

func uiaPointOffset(n SemanticNode, p image.Point) int {
	if line, ok := n.closestLine(p); ok {
		return line.nearest(n.Value, p.X)
	}
	if p.Y >= n.Bounds.Max.Y {
		return len(n.Value)
	}
	return 0
}

func uiaVisibleBoxes(n SemanticNode, r inputtext.Range) []image.Rectangle {
	var visible []image.Rectangle
	for _, box := range n.rangeBoxes(r) {
		if clipped := box.Intersect(n.Bounds); !clipped.Empty() {
			visible = append(visible, clipped)
		}
	}
	return visible
}
