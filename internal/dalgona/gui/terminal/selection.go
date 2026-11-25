package terminal

import (
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dalgona/vtstate"
)

type SelectionMode uint8

const (
	SelectCharacters SelectionMode = iota
	SelectWords
	SelectLines
	SelectRectangle
)

type Point struct {
	Line   int64
	Column int
}

func (p Point) before(o Point) bool {
	return p.Line < o.Line || (p.Line == o.Line && p.Column < o.Column)
}

type Selection struct {
	Anchor Point
	Head   Point
	Mode   SelectionMode
}

type Extent struct {
	Start     Point
	End       Point
	Rectangle bool
}

func (e Extent) Covers(line int64, column int) bool {
	switch {
	case line < e.Start.Line || line > e.End.Line:
		return false
	case e.Rectangle:
		return column >= e.Start.Column && column <= e.End.Column
	case line == e.Start.Line && column < e.Start.Column:
		return false
	}
	return line != e.End.Line || column <= e.End.Column
}

type wordClass uint8

const (
	classSpace wordClass = iota
	classWord
	classOther
)

const wordPunctuation = "_-./~:@%+#?&="

func classify(text string) wordClass {
	r, _ := utf8.DecodeRuneInString(text)
	switch {
	case text == " " || text == "":
		return classSpace
	case unicode.IsLetter(r) || unicode.IsDigit(r) || unicode.IsMark(r) || strings.ContainsRune(wordPunctuation, r):
		return classWord
	}
	return classOther
}

type cursorWalk struct {
	r       *Replica
	cells   []vtstate.Cell
	line    int64
	wrapped bool
	ok      bool
}

func (w *cursorWalk) load(line int64) bool {
	w.cells, w.wrapped, w.ok = w.r.AppendLine(w.cells[:0], line)
	w.line = line
	return w.ok
}

func (r *Replica) Extent(s Selection) Extent {
	first, end := r.Lines()
	start, stop := clampPoint(s.Anchor, first, end), clampPoint(s.Head, first, end)
	if stop.before(start) {
		start, stop = stop, start
	}
	if s.Mode == SelectRectangle {
		left, right := min(start.Column, stop.Column), max(start.Column, stop.Column)
		return Extent{Start: Point{start.Line, left}, End: Point{stop.Line, right}, Rectangle: true}
	}
	w := &cursorWalk{r: r}
	switch s.Mode {
	case SelectWords:
		return Extent{Start: w.wordStart(start), End: w.wordEnd(stop)}
	case SelectLines:
		return Extent{Start: w.logicalStart(start.Line), End: w.logicalEnd(stop.Line)}
	case SelectCharacters, SelectRectangle:
		return Extent{Start: w.cellStart(start), End: w.cellEnd(stop)}
	}
	return Extent{Start: w.cellStart(start), End: w.cellEnd(stop)}
}

func clampPoint(p Point, first, end int64) Point {
	p.Line = max(first, min(p.Line, end-1))
	p.Column = max(p.Column, 0)
	return p
}

func (w *cursorWalk) cellStart(p Point) Point {
	if w.load(p.Line) {
		p.Column = min(p.Column, len(w.cells)-1)
		if p.Column > 0 && w.cells[p.Column].Continuation() {
			p.Column--
		}
	}
	return p
}

func (w *cursorWalk) cellEnd(p Point) Point {
	if w.load(p.Line) {
		p.Column = min(p.Column, len(w.cells)-1)
		if w.cells[p.Column].Width == 2 {
			p.Column++
		}
	}
	return p
}

func (w *cursorWalk) wordStart(p Point) Point {
	p = w.cellStart(p)
	if !w.ok {
		return p
	}
	class := classify(w.cells[p.Column].Text)
	for {
		previous, ok := w.previousCell(p)
		if !ok || classify(w.cells[previous.Column].Text) != class {
			w.load(p.Line)
			return p
		}
		p = previous
	}
}

func (w *cursorWalk) previousCell(p Point) (Point, bool) {
	column := p.Column - 1
	if column < 0 {
		if !w.load(p.Line-1) || !w.wrapped {
			return p, false
		}
		column = len(w.cells) - 1
		p.Line--
	}
	if column > 0 && w.cells[column].Continuation() {
		column--
	}
	return Point{p.Line, column}, true
}

func (w *cursorWalk) wordEnd(p Point) Point {
	p = w.cellEnd(p)
	if !w.ok {
		return p
	}
	head := p.Column
	if w.cells[head].Continuation() {
		head--
	}
	class := classify(w.cells[head].Text)
	for {
		next, ok := w.nextCell(p)
		if !ok || classify(w.cells[next.Column].Text) != class {
			w.load(p.Line)
			return p
		}
		p = next
		if w.cells[p.Column].Width == 2 {
			p.Column++
		}
	}
}

func (w *cursorWalk) nextCell(p Point) (Point, bool) {
	column := p.Column + 1
	if column >= len(w.cells) {
		if !w.wrapped || !w.load(p.Line+1) {
			return p, false
		}
		return Point{p.Line + 1, 0}, true
	}
	return Point{p.Line, column}, true
}

func (w *cursorWalk) logicalStart(line int64) Point {
	for w.load(line-1) && w.wrapped {
		line--
	}
	return Point{Line: line}
}

func (w *cursorWalk) logicalEnd(line int64) Point {
	for w.load(line) && w.wrapped {
		if !w.load(line + 1) {
			break
		}
		line++
	}
	columns, _ := w.r.Size()
	return Point{Line: line, Column: columns - 1}
}

func (r *Replica) Text(e Extent) string {
	var b strings.Builder
	var cells []vtstate.Cell
	for line := e.Start.Line; line <= e.End.Line; line++ {
		var wrapped, ok bool
		cells, wrapped, ok = r.AppendLine(cells[:0], line)
		if !ok {
			continue
		}
		to := len(cells) - 1
		var from int
		if e.Rectangle {
			from, to = e.Start.Column, min(e.End.Column, to)
		} else {
			from, to = lineRange(e, line, to)
		}
		last := line == e.End.Line
		segment := cellText(cells, from, to)
		if wrapped && !last && !e.Rectangle {
			b.WriteString(segment)
			continue
		}
		b.WriteString(strings.TrimRight(segment, " "))
		if !last {
			b.WriteByte('\n')
		}
	}
	return b.String()
}

func lineRange(e Extent, line int64, last int) (from, to int) {
	to = last
	if line == e.Start.Line {
		from = e.Start.Column
	}
	if line == e.End.Line {
		to = min(e.End.Column, last)
	}
	return from, to
}

func cellText(cells []vtstate.Cell, from, to int) string {
	var b strings.Builder
	for x := max(from, 0); x <= to && x < len(cells); x++ {
		b.WriteString(cells[x].Text)
	}
	return b.String()
}
