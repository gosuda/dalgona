package document

import (
	"fmt"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

func (d *Document) Position(offset int, affinity Affinity) (Position, error) {
	if err := d.checkOffset(offset, affinity); err != nil {
		return Position{}, err
	}
	i, t := d.projected(offset, affinity)
	return d.at(i, snap(d.blocks[i].Projection, t, affinity), affinity), nil
}

func (d *Document) checkOffset(offset int, affinity Affinity) error {
	if offset < 0 || offset > len(d.source) || (offset < len(d.source) && !utf8.RuneStart(d.source[offset])) {
		return fmt.Errorf("%w: byte %d is not a character boundary of %d source bytes", textinput.InvalidOffset, offset, len(d.source))
	}
	if affinity != Before && affinity != After {
		return fmt.Errorf("%w: affinity %d is neither before nor after", textinput.InvalidOffset, affinity)
	}
	if len(d.blocks) == 0 {
		return fmt.Errorf("%w: %+v", ErrNoPosition, d.header.Identity)
	}
	return nil
}

func (d *Document) projected(offset int, affinity Affinity) (int, int) {
	i, _ := d.blockAt(offset)
	b := d.blocks[i]
	if offset < b.Span.Start {
		return i, 0
	}
	t := b.Projection.textOffset(offset, affinity)
	if next, ok := d.spillForward(i, t, offset, affinity); ok {
		return next, 0
	}
	if prev, prevT, ok := d.spillBackward(i, t, offset, affinity); ok {
		return prev, prevT
	}
	return i, t
}

func (d *Document) spillForward(i, t, offset int, affinity Affinity) (int, bool) {
	if affinity != After || t != len(d.blocks[i].Projection.Text) {
		return 0, false
	}
	if i+1 < len(d.blocks) && offset > d.contentEnd(i) {
		return i + 1, true
	}
	return 0, false
}

func (d *Document) spillBackward(i, t, offset int, affinity Affinity) (int, int, bool) {
	if affinity != Before || t != 0 || i == 0 {
		return 0, 0, false
	}
	if offset <= d.contentEnd(i-1) {
		return i - 1, len(d.blocks[i-1].Projection.Text), true
	}
	return 0, 0, false
}

func (d *Document) contentEnd(i int) int {
	runs := d.blocks[i].Projection.Runs
	if len(runs) == 0 {
		return d.blocks[i].Span.Start
	}
	return runs[len(runs)-1].Source.End
}

func (d *Document) at(i, t int, affinity Affinity) Position {
	b := d.blocks[i]
	offset := b.Span.Start
	if len(b.Projection.Runs) > 0 {
		offset = b.Projection.sourceOffset(t, affinity)
	}
	return Position{Document: d.header.Identity, Block: b.Key, Offset: offset, Affinity: affinity}
}

func snap(p Projection, t int, affinity Affinity) int {
	if run, inside := p.insideAtomic(t); inside {
		if affinity == Before {
			return run.Text.End
		}
		return run.Text.Start
	}
	cluster, err := textinput.PaintRange(p.Text, textinput.Range{Start: t, End: t})
	if err != nil || cluster.Start == cluster.End {
		return t
	}
	if affinity == Before {
		return snap(p, cluster.End, affinity)
	}
	return snap(p, cluster.Start, affinity)
}

func (d *Document) locate(p Position) (int, int, error) {
	if p.Document != d.header.Identity {
		return 0, 0, fmt.Errorf("%w: %+v is not %+v", ErrForeign, p.Document, d.header.Identity)
	}
	resolved, err := d.Position(p.Offset, p.Affinity)
	if err != nil {
		return 0, 0, err
	}
	i, _ := d.blockAt(resolved.Offset)
	if i > 0 && d.blocks[i].Key != resolved.Block && d.blocks[i-1].Key == resolved.Block {
		i--
	}
	return i, d.blocks[i].Projection.textOffset(resolved.Offset, resolved.Affinity), nil
}

func (d *Document) Remap(change Change, p Position) (Position, error) {
	if p.Document != change.From {
		return Position{}, fmt.Errorf("%w: %+v is not %+v", ErrForeign, p.Document, change.From)
	}
	if change.Revision != d.revision || change.To != d.header.Identity {
		return Position{}, fmt.Errorf("%w: change for revision %d does not produce revision %d", ErrStale, change.Revision, d.revision)
	}
	offset := p.Offset
	if !change.Appended {
		offset = mapOffset(change.Edits, offset, p.Affinity)
	}
	return d.Position(offset, p.Affinity)
}

func (d *Document) Start() (Position, error) {
	return d.Position(0, After)
}

func (d *Document) End() (Position, error) {
	return d.Position(len(d.source), Before)
}

func (d *Document) TextOffset(p Position, unit textinput.Unit) (Key, int, error) {
	i, t, err := d.locate(p)
	if err != nil {
		return Key{}, 0, err
	}
	converted, err := textinput.Convert(d.blocks[i].Projection.Text, t, textinput.Bytes, unit)
	return d.blocks[i].Key, converted, err
}
