package platform

import (
	"errors"
	"fmt"
	"image"
	"iter"
	"math"
	"sort"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

type TextLine struct {
	Range    inputtext.Range
	Bounds   image.Rectangle
	Clusters []ClusterSpan
}

type ClusterSpan struct {
	Left, Right int
}

func checkLines(text string, lines []TextLine) error {
	for i, line := range lines {
		if i > 0 && (line.Range.Start <= lines[i-1].Range.Start || line.Range.Start < lines[i-1].Range.End) {
			return fmt.Errorf("text line %d overlaps or precedes line %d; publish lines in text order", i, i-1)
		}
		if err := line.check(text); err != nil {
			return fmt.Errorf("text line %d: %w", i, err)
		}
	}
	return nil
}

func (l TextLine) check(text string) error {
	if inputtext.CheckRange(text, l.Range) != nil {
		return errors.New("its range leaves the text or splits a grapheme cluster")
	}
	if l.Bounds != l.Bounds.Canon() {
		return errors.New("its box has a minimum beyond its maximum")
	}
	if clusters := clusterCount(text, l.Range); clusters != len(l.Clusters) {
		return fmt.Errorf("it has %d cluster spans for %d grapheme clusters", len(l.Clusters), clusters)
	}
	for k, span := range l.Clusters {
		if span.Left > span.Right || span.Left < l.Bounds.Min.X || span.Right > l.Bounds.Max.X {
			return fmt.Errorf("cluster span %d leaves the line box or ends before it starts", k)
		}
	}
	return nil
}

func clusterCount(text string, r inputtext.Range) int {
	count := 0
	for at := r.Start; at < r.End; count++ {
		next, err := inputtext.NextBoundary(text, at)
		if err != nil || next <= at {
			return -1
		}
		at = next
	}
	return count
}

func (l TextLine) clusters(text string) iter.Seq2[inputtext.Range, ClusterSpan] {
	return func(yield func(inputtext.Range, ClusterSpan) bool) {
		start := l.Range.Start
		for _, span := range l.Clusters {
			end, err := inputtext.NextBoundary(text, start)
			if err != nil || !yield(inputtext.Range{Start: start, End: end}, span) {
				return
			}
			start = end
		}
	}
}

func (l TextLine) column(span ClusterSpan) image.Rectangle {
	return image.Rectangle{Min: image.Pt(span.Left, l.Bounds.Min.Y), Max: image.Pt(span.Right, l.Bounds.Max.Y)}
}

func (l TextLine) end() ClusterSpan {
	if len(l.Clusters) == 0 {
		return ClusterSpan{Left: l.Bounds.Min.X, Right: l.Bounds.Min.X}
	}
	first, last := l.Clusters[0], l.Clusters[len(l.Clusters)-1]
	if last.Left < first.Left {
		return ClusterSpan{Left: last.Left, Right: last.Left}
	}
	return ClusterSpan{Left: last.Right, Right: last.Right}
}

func (l TextLine) portion(text string, r inputtext.Range) (image.Rectangle, bool) {
	span := ClusterSpan{Left: math.MaxInt, Right: math.MinInt}
	for cluster, s := range l.clusters(text) {
		if cluster.Start < r.End && r.Start < cluster.End {
			span = ClusterSpan{Left: min(span.Left, s.Left), Right: max(span.Right, s.Right)}
		}
	}
	return l.column(span), span.Left <= span.Right
}

func (l TextLine) nearest(text string, x int) int {
	best, distance := l.Range.Start, math.MaxInt
	for cluster, span := range l.clusters(text) {
		if d := max(span.Left-x, x-span.Right+1, 0); d < distance {
			best, distance = cluster.Start, d
		}
	}
	return best
}

func endsWithBreak(text string, r inputtext.Range) bool {
	if r.Start == r.End {
		return false
	}
	last, err := inputtext.PreviousBoundary(text, r.End)
	return err == nil && lineBreak(text[last:])
}

func lineBreak(text string) bool {
	r, _ := utf8.DecodeRuneInString(text)
	return r >= '\n' && r <= '\r' || r == '\u0085' || r == '\u2028' || r == '\u2029'
}

func (n SemanticNode) lineAt(offset int) (TextLine, bool) {
	i := sort.Search(len(n.Lines), func(i int) bool { return n.Lines[i].Range.Start > offset }) - 1
	if i < 0 {
		return TextLine{}, false
	}
	line := n.Lines[i]
	last := offset == len(n.Value) && offset == line.Range.End && (line.Range.Start == offset || !endsWithBreak(n.Value, line.Range))
	return line, offset < line.Range.End || last
}

func (n SemanticNode) lineAround(offset int) (TextLine, bool) {
	if line, ok := n.lineAt(offset); ok {
		return line, true
	}
	hard := hardLine(n.Value, offset)
	next := sort.Search(len(n.Lines), func(i int) bool { return n.Lines[i].Range.Start > offset })
	if next < len(n.Lines) {
		hard.End = min(hard.End, n.Lines[next].Range.Start)
	}
	if next > 0 {
		hard.Start = max(hard.Start, n.Lines[next-1].Range.End)
	}
	return TextLine{Range: hard}, false
}

func hardLine(text string, offset int) inputtext.Range {
	at, err := inputtext.PaintRange(text, inputtext.Range{Start: offset, End: offset})
	if err != nil {
		return inputtext.Range{Start: offset, End: offset}
	}
	start := at.Start
	for start > 0 {
		previous, err := inputtext.PreviousBoundary(text, start)
		if err != nil || lineBreak(text[previous:]) {
			break
		}
		start = previous
	}
	end := at.Start
	for end < len(text) {
		next, err := inputtext.NextBoundary(text, end)
		if err != nil {
			break
		}
		broke := lineBreak(text[end:])
		end = next
		if broke {
			break
		}
	}
	return inputtext.Range{Start: start, End: end}
}

func (n SemanticNode) textLines() iter.Seq2[TextLine, bool] {
	return func(yield func(TextLine, bool) bool) {
		for at := 0; ; {
			line, laidOut := n.lineAround(at)
			if !yield(line, laidOut) || line.Range.End == len(n.Value) && !endsWithBreak(n.Value, line.Range) {
				return
			}
			at = line.Range.End
		}
	}
}

func (n SemanticNode) characterBox(offset int) (image.Rectangle, bool) {
	line, ok := n.lineAt(offset)
	if !ok {
		return image.Rectangle{}, false
	}
	for cluster, span := range line.clusters(n.Value) {
		if offset < cluster.End {
			return line.column(span), true
		}
	}
	return line.column(line.end()), true
}

func (n SemanticNode) rangeBoxes(r inputtext.Range) []image.Rectangle {
	if r.Start >= r.End || n.Bounds.Empty() && len(n.Lines) == 0 {
		return nil
	}
	if len(n.Lines) == 0 {
		return []image.Rectangle{n.Bounds}
	}
	var boxes []image.Rectangle
	first := sort.Search(len(n.Lines), func(i int) bool { return n.Lines[i].Range.End > r.Start })
	for _, line := range n.Lines[first:] {
		if line.Range.Start >= r.End {
			break
		}
		if box, ok := line.portion(n.Value, r); ok {
			boxes = append(boxes, box)
		}
	}
	return boxes
}

func (n SemanticNode) rangeBox(r inputtext.Range) (image.Rectangle, bool) {
	boxes := n.rangeBoxes(r)
	if len(boxes) == 0 {
		return image.Rectangle{}, false
	}
	box := boxes[0]
	for _, b := range boxes[1:] {
		box = image.Rectangle{Min: image.Pt(min(box.Min.X, b.Min.X), min(box.Min.Y, b.Min.Y)), Max: image.Pt(max(box.Max.X, b.Max.X), max(box.Max.Y, b.Max.Y))}
	}
	return box, true
}

func (n SemanticNode) closestLine(p image.Point) (TextLine, bool) {
	best, distance := -1, math.MaxInt
	for i, line := range n.Lines {
		if d := max(line.Bounds.Min.Y-p.Y, p.Y-line.Bounds.Max.Y+1, 0); d < distance {
			best, distance = i, d
		}
	}
	if best < 0 {
		return TextLine{}, false
	}
	return n.Lines[best], true
}

func letterWord(segment string) bool {
	return strings.ContainsFunc(segment, func(r rune) bool { return unicode.IsLetter(r) || unicode.IsNumber(r) })
}

func visibleWord(segment string) bool {
	return strings.ContainsFunc(segment, func(r rune) bool { return !unicode.IsSpace(r) })
}

func nextWordStart(text string, at int, starts func(segment string) bool) (int, error) {
	for b := at; b < len(text); {
		next, err := inputtext.NextWordBoundary(text, b)
		if err != nil {
			return 0, err
		}
		if b > at && starts(text[b:next]) {
			return b, nil
		}
		b = next
	}
	return len(text), nil
}

func previousWordStart(text string, at int, starts func(segment string) bool) (int, error) {
	end, err := inputtext.NextWordBoundary(text, at)
	for err == nil && end > 0 {
		start, previousErr := inputtext.PreviousWordBoundary(text, end)
		if previousErr == nil && starts(text[start:end]) {
			return start, nil
		}
		end, err = start, previousErr
	}
	return 0, err
}

func wordSegment(text string, at int, starts func(segment string) bool) (inputtext.Range, error) {
	start, err := previousWordStart(text, at, starts)
	if err != nil {
		return inputtext.Range{}, err
	}
	end, err := nextWordStart(text, at, starts)
	if err != nil {
		return inputtext.Range{}, err
	}
	return inputtext.PaintRange(text, inputtext.Range{Start: start, End: end})
}

func sentenceSegment(text string, at int) (inputtext.Range, error) {
	end, err := inputtext.NextSentenceBoundary(text, at)
	if err != nil {
		return inputtext.Range{}, err
	}
	start, err := inputtext.PreviousSentenceBoundary(text, end)
	if err != nil {
		return inputtext.Range{}, err
	}
	return inputtext.PaintRange(text, inputtext.Range{Start: start, End: end})
}
