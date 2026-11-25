package document

import (
	"errors"
	"fmt"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

var ErrNoLayout = errors.New("document row has no layout; load the document and lay out the row, then retry")

type Layout func(row int) (*Flow, bool)

func (d *Document) At(block, offset int, affinity Affinity) (Position, error) {
	if block < 0 || block >= len(d.blocks) {
		return Position{}, fmt.Errorf("%w: block %d of %d", ErrNoPosition, block, len(d.blocks))
	}
	p := d.blocks[block].Projection
	if offset < 0 || offset > len(p.Text) {
		return Position{}, fmt.Errorf("%w: projected offset %d of %d", textinput.InvalidOffset, offset, len(p.Text))
	}
	return d.at(block, snap(p, offset, affinity), affinity), nil
}

func (d *Document) LineSelection(block int, f *Flow, y float32) (Selection, error) {
	line := f.Lines[f.lineAt(y)]
	anchor, err := d.At(block, line.Text.Start, After)
	if err != nil {
		return Selection{}, err
	}
	focus, err := d.At(block, line.Text.End, Before)
	if err != nil {
		return Selection{}, err
	}
	return Selection{Anchor: anchor, Focus: focus}, nil
}

type spot struct {
	row      int
	flow     *Flow
	offset   int
	affinity Affinity
}

func (v *Viewport) spot(scope *Scope, p Position, layout Layout) (spot, error) {
	row, ok := v.Locate(scope, p)
	if !ok {
		return spot{}, fmt.Errorf("%w: %+v", ErrUnknown, p.Document)
	}
	f, ok := v.flow(row, layout)
	if !ok {
		return spot{}, fmt.Errorf("%w: row %d", ErrNoLayout, row)
	}
	block, t, err := scope.entries[v.rows[row].Entry].doc.locate(p)
	if err != nil {
		return spot{}, err
	}
	s := spot{row: row, flow: f, offset: t, affinity: p.Affinity}
	if block != v.rows[row].Block || t > f.Row.End {
		s.offset, s.affinity = f.Row.End, Before
	}
	if t < f.Row.Start {
		s.offset, s.affinity = f.Row.Start, After
	}
	return s, nil
}

func (v *Viewport) flow(row int, layout Layout) (*Flow, bool) {
	if v.rows[row].Block < 0 {
		return nil, false
	}
	return layout(row)
}

func (v *Viewport) position(scope *Scope, row, offset int, affinity Affinity) (Position, error) {
	ref := v.rows[row]
	return scope.entries[ref.Entry].doc.At(ref.Block, offset, affinity)
}

func (v *Viewport) CaretX(scope *Scope, p Position, layout Layout) (float32, error) {
	s, err := v.spot(scope, p, layout)
	if err != nil {
		return 0, err
	}
	x, _, _ := s.flow.Caret(s.offset, s.affinity)
	return x + s.flow.Inset, nil
}

func (v *Viewport) LineUp(scope *Scope, p Position, goal float32, layout Layout) (Position, bool, error) {
	return v.moveLine(scope, p, goal, -1, layout)
}

func (v *Viewport) LineDown(scope *Scope, p Position, goal float32, layout Layout) (Position, bool, error) {
	return v.moveLine(scope, p, goal, 1, layout)
}

func (v *Viewport) moveLine(scope *Scope, p Position, goal float32, step int, layout Layout) (Position, bool, error) {
	s, err := v.spot(scope, p, layout)
	if err != nil {
		return p, false, err
	}
	row, f, line := s.row, s.flow, s.flow.lineOf(s.offset, s.affinity)+step
	for line < 0 || line >= len(f.Lines) {
		row += step
		if row < 0 || row >= len(v.rows) {
			return p, false, nil
		}
		next, ok := v.flow(row, layout)
		if !ok {
			return p, false, fmt.Errorf("%w: row %d", ErrNoLayout, row)
		}
		f, line = next, 0
		if step < 0 {
			line = len(f.Lines) - 1
		}
	}
	target := f.Lines[line]
	offset, affinity := f.Hit(goal-f.Inset, (target.Top+target.Bottom)/2)
	moved, err := v.position(scope, row, offset, affinity)
	return moved, err == nil, err
}

func (v *Viewport) LineStart(scope *Scope, p Position, layout Layout) (Position, error) {
	s, err := v.spot(scope, p, layout)
	if err != nil {
		return p, err
	}
	line := s.flow.Lines[s.flow.lineOf(s.offset, s.affinity)]
	return v.position(scope, s.row, line.Text.Start, After)
}

func (v *Viewport) LineEnd(scope *Scope, p Position, layout Layout) (Position, error) {
	s, err := v.spot(scope, p, layout)
	if err != nil {
		return p, err
	}
	line := s.flow.Lines[s.flow.lineOf(s.offset, s.affinity)]
	return v.position(scope, s.row, s.flow.contentEnd(line), Before)
}
