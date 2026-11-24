//go:build gui

package terminal

import (
	"image"
	"image/color"

	"gioui.org/f32"
	"gioui.org/font"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/op/paint"
	"gioui.org/text"
	"gioui.org/unit"
	"golang.org/x/image/math/fixed"

	"github.com/gosuda/dalgona/internal/dalgona/vtstate"
)

type Painter struct {
	Shaper   *text.Shaper
	Palette  Palette
	Font     font.Font
	TextSize unit.Sp
}

type cellMetrics struct {
	size     image.Point
	baseline int
}

func (p *Painter) metrics(gtx layout.Context) cellMetrics {
	p.Shaper.LayoutString(p.params(gtx, p.Font), "M")
	var m cellMetrics
	for g, ok := p.Shaper.NextGlyph(); ok; g, ok = p.Shaper.NextGlyph() {
		m = cellMetrics{
			size:     image.Pt(max(g.Advance.Ceil(), 1), max((g.Ascent+g.Descent).Ceil(), 1)),
			baseline: g.Ascent.Ceil(),
		}
	}
	if m.size == (image.Point{}) {
		size := max(gtx.Sp(p.TextSize), 2)
		m = cellMetrics{size: image.Pt(size/2, size), baseline: size}
	}
	return m
}

func (p *Painter) params(gtx layout.Context, f font.Font) text.Parameters {
	return text.Parameters{
		Font:     f,
		PxPerEm:  fixed.I(gtx.Sp(p.TextSize)),
		MaxLines: 1,
		MaxWidth: 1 << 24,
		Locale:   gtx.Locale,
	}
}

func (p *Painter) Layout(gtx layout.Context, v *View) layout.Dimensions {
	m := p.metrics(gtx)
	size := image.Pt(m.size.X*v.Columns, m.size.Y*len(v.Lines))
	defer clip.Rect{Max: size}.Push(gtx.Ops).Pop()
	paint.FillShape(gtx.Ops, p.Palette.Background, clip.Rect{Max: size}.Op())
	for row, line := range v.Lines {
		origin := image.Pt(0, row*m.size.Y)
		p.backgrounds(gtx, v, line, origin, m)
		if line.ID == v.CursorLine {
			p.cursor(gtx, v.Cursor, origin, m)
		}
		p.glyphs(gtx, v, line, origin, m)
		if line.ID == v.First {
			paint.FillShape(gtx.Ops, p.Palette.Boundary, clip.Rect{Min: origin, Max: image.Pt(size.X, origin.Y+1)}.Op())
		}
	}
	return layout.Dimensions{Size: size}
}

func (p *Painter) backgrounds(gtx layout.Context, v *View, line Line, origin image.Point, m cellMetrics) {
	for x, c := range line.Cells {
		_, bg := p.Palette.colors(c.Style)
		if v.HasSelection && v.Selection.Covers(line.ID, x) {
			bg = p.Palette.Selection
		}
		if bg == p.Palette.Background {
			continue
		}
		cell := image.Rectangle{Min: origin.Add(image.Pt(x*m.size.X, 0)), Max: origin.Add(image.Pt((x+1)*m.size.X, m.size.Y))}
		paint.FillShape(gtx.Ops, bg, clip.Rect(cell).Op())
	}
}

func (p *Painter) cursor(gtx layout.Context, c vtstate.CursorState, origin image.Point, m cellMetrics) {
	if !c.Visible {
		return
	}
	cell := image.Rectangle{Min: origin.Add(image.Pt(c.Column*m.size.X, 0)), Max: origin.Add(image.Pt((c.Column+1)*m.size.X, m.size.Y))}
	thickness := max(gtx.Dp(2), 1)
	switch c.Shape {
	case vtstate.CursorUnderline:
		cell.Min.Y = cell.Max.Y - thickness
	case vtstate.CursorBar:
		cell.Max.X = cell.Min.X + thickness
	case vtstate.CursorBlock:
	}
	paint.FillShape(gtx.Ops, p.Palette.Cursor, clip.Rect(cell).Op())
}

func (p *Painter) glyphs(gtx layout.Context, v *View, line Line, origin image.Point, m cellMetrics) {
	for x, c := range line.Cells {
		if c.Continuation() || c.Text == " " {
			continue
		}
		fg, bg := p.Palette.colors(c.Style)
		if v.Cursor.Visible && v.Cursor.Shape == vtstate.CursorBlock && line.ID == v.CursorLine && x == v.Cursor.Column {
			fg = bg
		}
		at := origin.Add(image.Pt(x*m.size.X, 0))
		p.text(gtx, c, at, fg, m)
		p.decorate(gtx, c, at, fg, m)
	}
}

func (p *Painter) text(gtx layout.Context, c vtstate.Cell, at image.Point, fg color.NRGBA, m cellMetrics) {
	f := p.Font
	if c.Style.Attributes&vtstate.AttrBold != 0 {
		f.Weight = font.Bold
	}
	if c.Style.Attributes&vtstate.AttrItalic != 0 {
		f.Style = font.Italic
	}
	p.Shaper.LayoutString(p.params(gtx, f), c.Text)
	var buf [8]text.Glyph
	glyphs := buf[:0]
	for g, ok := p.Shaper.NextGlyph(); ok; g, ok = p.Shaper.NextGlyph() {
		glyphs = append(glyphs, g)
	}
	if len(glyphs) == 0 {
		return
	}
	first := glyphs[0]
	offset := f32.Pt(float32(at.X)+float32(first.X)/64, float32(at.Y+m.baseline))
	defer op.Affine(f32.AffineId().Offset(offset)).Push(gtx.Ops).Pop()
	outline := clip.Outline{Path: p.Shaper.Shape(glyphs)}.Op().Push(gtx.Ops)
	paint.ColorOp{Color: fg}.Add(gtx.Ops)
	paint.PaintOp{}.Add(gtx.Ops)
	outline.Pop()
	if call := p.Shaper.Bitmaps(glyphs); call != (op.CallOp{}) {
		call.Add(gtx.Ops)
	}
}

func (p *Painter) decorate(gtx layout.Context, c vtstate.Cell, at image.Point, fg color.NRGBA, m cellMetrics) {
	width := m.size.X * int(c.Width)
	line := max(gtx.Dp(1), 1)
	if c.Style.Underline != vtstate.UnderlineNone || c.Link != nil {
		ink := p.Palette.resolve(c.Style.UnderlineColor, fg)
		y := at.Y + m.baseline + line
		paint.FillShape(gtx.Ops, ink, clip.Rect{Min: image.Pt(at.X, y), Max: image.Pt(at.X+width, y+line)}.Op())
		if c.Style.Underline == vtstate.UnderlineDouble {
			y += 2 * line
			paint.FillShape(gtx.Ops, ink, clip.Rect{Min: image.Pt(at.X, y), Max: image.Pt(at.X+width, y+line)}.Op())
		}
	}
	if c.Style.Attributes&vtstate.AttrStrikethrough != 0 {
		y := at.Y + m.size.Y/2
		paint.FillShape(gtx.Ops, fg, clip.Rect{Min: image.Pt(at.X, y), Max: image.Pt(at.X+width, y+line)}.Op())
	}
}
