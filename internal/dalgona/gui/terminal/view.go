package terminal

import (
	"math"

	"github.com/gosuda/dalgona/internal/dalgona/vtstate"
)

type Line struct {
	Cells   []vtstate.Cell
	ID      int64
	Wrapped bool
}

type View struct {
	Lines        []Line
	Selection    Extent
	Cursor       vtstate.CursorState
	CursorLine   int64
	First        int64
	Columns      int
	HasSelection bool
}

func (r *Replica) span() (first, history int64) {
	pushed := r.term.ScrollbackPushed()
	if pushed > uint64(math.MaxInt64) {
		pushed = uint64(math.MaxInt64)
	}
	first = int64(pushed)
	if r.term.AlternateScreen() {
		return first, 0
	}
	retained := int64(r.term.ScrollbackLen())
	return first - retained, retained
}

func (r *Replica) Lines() (first, end int64) {
	first, history := r.span()
	_, rows := r.term.Size()
	return first, first + history + int64(rows)
}

func (r *Replica) ScreenTop() int64 {
	first, history := r.span()
	return first + history
}

func (r *Replica) AppendLine(dst []vtstate.Cell, id int64) (cells []vtstate.Cell, wrapped, ok bool) {
	first, history := r.span()
	columns, rows := r.term.Size()
	offset := id - first
	switch {
	case offset < 0 || offset >= history+int64(rows):
		return dst, false, false
	case offset < history:
		start := len(dst)
		dst = r.term.AppendScrollback(dst, int(offset))
		for len(dst)-start < columns {
			dst = append(dst, vtstate.Cell{Text: " ", Width: 1})
		}
		return dst, r.term.ScrollbackWrapped(int(offset)), true
	default:
		y := int(offset - history)
		return r.term.AppendRow(dst, y), r.term.RowWrapped(y), true
	}
}

func (r *Replica) Window(v *View, top int64, height int) {
	first, end := r.Lines()
	top = max(first, min(top, end-int64(height)))
	columns, _ := r.term.Size()
	v.Columns, v.First = columns, first
	lines := v.Lines[:0]
	for id := top; id < min(top+int64(height), end); id++ {
		var line Line
		if len(lines) < cap(lines) {
			line = lines[:len(lines)+1][len(lines)]
		}
		line.ID = id
		line.Cells, line.Wrapped, _ = r.AppendLine(line.Cells[:0], id)
		lines = append(lines, line)
	}
	v.Lines = lines
	v.Cursor = r.term.Cursor()
	v.CursorLine = r.ScreenTop() + int64(v.Cursor.Row)
}
