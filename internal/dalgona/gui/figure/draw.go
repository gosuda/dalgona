//go:build gui

package figure

import (
	"image"
	"image/color"
	"math"

	"gioui.org/f32"
	giofont "gioui.org/font"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/op/paint"
	"gioui.org/text"
	giounit "gioui.org/unit"
	"gioui.org/widget"
)

type Faces struct {
	Sans giofont.Typeface
	Mono giofont.Typeface
}

type Canvas struct {
	List   *List
	Shaper *text.Shaper
	Faces  Faces
	Zoom   float32
	images []paint.ImageOp
}

func NewCanvas(l *List, shaper *text.Shaper, faces Faces) *Canvas {
	c := &Canvas{List: l, Shaper: shaper, Faces: faces, Zoom: 1}
	for _, t := range l.Tiles {
		img := &image.RGBA{Pix: t.Pix, Stride: t.Width * 4, Rect: image.Rect(0, 0, t.Width, t.Height)}
		c.images = append(c.images, paint.NewImageOp(img))
	}
	return c
}

func (c *Canvas) Layout(gtx layout.Context) layout.Dimensions {
	factor := gtx.Metric.PxPerDp * c.Zoom
	size := image.Pt(int(c.List.Width*factor+0.5), int(c.List.Height*factor+0.5))
	defer clip.Rect{Max: size}.Push(gtx.Ops).Pop()
	defer op.Affine(f32.Affine2D{}.Scale(f32.Point{}, f32.Pt(factor, factor))).Push(gtx.Ops).Pop()
	var stack []func()
	for _, o := range c.List.Ops {
		if o.Kind == OpPop {
			if n := len(stack); n > 0 {
				stack[n-1]()
				stack = stack[:n-1]
			}
			continue
		}
		if pop := c.op(gtx, o); pop != nil {
			stack = append(stack, pop)
		}
	}
	for i := len(stack) - 1; i >= 0; i-- {
		stack[i]()
	}
	return layout.Dimensions{Size: size}
}

func gioAffine(m Affine) f32.Affine2D { return f32.NewAffine2D(m.A, m.C, m.E, m.B, m.D, m.F) }

func (c *Canvas) op(gtx layout.Context, o Op) func() {
	switch o.Kind {
	case OpTransform:
		return op.Affine(gioAffine(o.Transform)).Push(gtx.Ops).Pop
	case OpClip:
		return clip.Outline{Path: c.path(gtx.Ops, o.Index)}.Op().Push(gtx.Ops).Pop
	case OpOpacity:
		return paint.PushOpacity(gtx.Ops, o.Alpha).Pop
	case OpFill:
		c.fill(gtx.Ops, o)
	case OpText:
		c.text(gtx, c.List.Chunks[o.Index])
	case OpImage:
		c.image(gtx.Ops, o.Index)
	case OpPop:
	}
	return nil
}

func (c *Canvas) path(ops *op.Ops, index int32) clip.PathSpec {
	p := c.List.Paths[index]
	var b clip.Path
	b.Begin(ops)
	k := 0
	for _, v := range p.Verbs {
		pt := func(i int) f32.Point { return f32.Pt(p.Points[k+i].X, p.Points[k+i].Y) }
		switch v {
		case VerbMove:
			b.MoveTo(pt(0))
		case VerbLine:
			b.LineTo(pt(0))
		case VerbQuad:
			b.QuadTo(pt(0), pt(1))
		case VerbCube:
			b.CubeTo(pt(0), pt(1), pt(2))
		case VerbClose:
			b.Close()
		}
		k += verbPoints[v]
	}
	return b.End()
}

func nrgba(c Color) color.NRGBA { return color.NRGBA{R: c.R, G: c.G, B: c.B, A: c.A} }

func (c *Canvas) fill(ops *op.Ops, o Op) {
	defer clip.Outline{Path: c.path(ops, o.Index)}.Op().Push(ops).Pop()
	if o.Paint.Gradient < 0 {
		paint.ColorOp{Color: nrgba(o.Paint.Color)}.Add(ops)
		paint.PaintOp{}.Add(ops)
		return
	}
	c.gradient(ops, c.List.Gradients[o.Paint.Gradient])
}

const gradientReach = CoordinateLimit

const maxSpreadPeriods = 128

func (c *Canvas) gradient(ops *op.Ops, g Gradient) {
	defer op.Affine(gioAffine(g.Space)).Push(ops).Pop()
	if g.Start == g.End {
		paint.ColorOp{Color: nrgba(g.Stops[len(g.Stops)-1].Color)}.Add(ops)
		paint.PaintOp{}.Add(ops)
		return
	}
	minT, maxT := c.spreadRange(g)
	for _, s := range spreadStrips(g, minT, maxT) {
		c.band(ops, g, s)
	}
}

func (c *Canvas) spreadRange(g Gradient) (float32, float32) {
	axis := f32.Pt(g.End.X-g.Start.X, g.End.Y-g.Start.Y)
	den := axis.X*axis.X + axis.Y*axis.Y
	inv, ok := g.Space.invert()
	project := func(p f32.Point) float32 {
		if ok {
			q := inv.Apply(Point{X: p.X, Y: p.Y})
			p = f32.Pt(q.X, q.Y)
		}
		return ((p.X-g.Start.X)*axis.X + (p.Y-g.Start.Y)*axis.Y) / den
	}
	minT, maxT := float32(math.Inf(1)), float32(math.Inf(-1))
	for _, corner := range [...]f32.Point{{}, {X: c.List.Width}, {Y: c.List.Height}, {X: c.List.Width, Y: c.List.Height}} {
		t := project(corner)
		minT, maxT = min(minT, t), max(maxT, t)
	}
	return minT, maxT
}

func spreadStrips(g Gradient, minT, maxT float32) []gradientStrip {
	first, last := g.Stops[0], g.Stops[len(g.Stops)-1]
	middle := middleStrips(g)
	pad := edgeStrips(first, last, middle)
	if g.Spread == SpreadPad || len(middle) == 0 {
		return pad
	}
	if repeated, ok := periodicStrips(g, middle, minT, maxT); ok {
		return repeated
	}
	return pad
}

func middleStrips(g Gradient) []gradientStrip {
	middle := make([]gradientStrip, 0, len(g.Stops)-1)
	for i := 1; i < len(g.Stops); i++ {
		middle = append(middle, gradientStrip{from: g.Stops[i-1].Offset, to: g.Stops[i].Offset, start: g.Stops[i-1], end: g.Stops[i]})
	}
	return middle
}

func edgeStrips(first, last Stop, middle []gradientStrip) []gradientStrip {
	pad := make([]gradientStrip, 0, len(middle)+2)
	pad = append(pad, gradientStrip{from: -gradientReach, to: first.Offset, start: first, end: first})
	pad = append(pad, middle...)
	return append(pad, gradientStrip{from: last.Offset, to: gradientReach, start: last, end: last})
}

func periodicStrips(g Gradient, middle []gradientStrip, minT, maxT float32) ([]gradientStrip, bool) {
	lo, hi := int(math.Floor(float64(minT)))-1, int(math.Ceil(float64(maxT)))+1
	if hi-lo > maxSpreadPeriods || lo > hi {
		return nil, false
	}
	out := make([]gradientStrip, 0, (hi-lo+1)*(len(middle)+2))
	for k := lo; k <= hi; k++ {
		out = append(out, periodStrips(g, middle, float32(k), k%2 != 0)...)
	}
	return out, true
}

func edgeBand(from, to float32, stop Stop) []gradientStrip {
	if to <= from {
		return nil
	}
	return []gradientStrip{{from: from, to: to, start: stop, end: stop}}
}

func periodStrips(g Gradient, middle []gradientStrip, shift float32, mirror bool) []gradientStrip {
	first, last := g.Stops[0], g.Stops[len(g.Stops)-1]
	if g.Spread == SpreadReflect && mirror {
		out := make([]gradientStrip, 0, len(middle)+2)
		out = append(out, edgeBand(shift, shift+1-last.Offset, last)...)
		for i := len(middle) - 1; i >= 0; i-- {
			m := middle[i]
			out = append(out, gradientStrip{from: shift + 1 - m.to, to: shift + 1 - m.from, start: m.end, end: m.start})
		}
		out = append(out, edgeBand(shift+1-first.Offset, shift+1, first)...)
		return out
	}
	out := make([]gradientStrip, 0, len(middle)+2)
	out = append(out, edgeBand(shift, shift+first.Offset, first)...)
	for _, m := range middle {
		out = append(out, gradientStrip{from: shift + m.from, to: shift + m.to, start: m.start, end: m.end})
	}
	out = append(out, edgeBand(shift+last.Offset, shift+1, last)...)
	return out
}

type gradientStrip struct {
	from, to   float32
	start, end Stop
}

func (c *Canvas) band(ops *op.Ops, g Gradient, s gradientStrip) {
	if s.to <= s.from {
		return
	}
	axis := f32.Pt(g.End.X-g.Start.X, g.End.Y-g.Start.Y)
	normal := f32.Pt(-axis.Y, axis.X).Mul(gradientReach / max(1e-6, axisLength(axis)))
	at := func(t float32) f32.Point { return f32.Pt(g.Start.X, g.Start.Y).Add(axis.Mul(t)) }
	var b clip.Path
	b.Begin(ops)
	b.MoveTo(at(s.from).Add(normal))
	b.LineTo(at(s.to).Add(normal))
	b.LineTo(at(s.to).Sub(normal))
	b.LineTo(at(s.from).Sub(normal))
	b.Close()
	defer clip.Outline{Path: b.End()}.Op().Push(ops).Pop()
	paint.LinearGradientOp{Stop1: at(s.start.Offset), Color1: nrgba(s.start.Color), Stop2: at(s.end.Offset), Color2: nrgba(s.end.Color)}.Add(ops)
	paint.PaintOp{}.Add(ops)
}

func axisLength(p f32.Point) float32 {
	return float32(math.Hypot(float64(p.X), float64(p.Y)))
}

func (c *Canvas) image(ops *op.Ops, index int32) {
	t := c.List.Tiles[index]
	scale := f32.Pt(t.Bounds.Width()/float32(t.Width), t.Bounds.Height()/float32(t.Height))
	placement := f32.Affine2D{}.Scale(f32.Point{}, scale).Offset(f32.Pt(t.Bounds.Min.X, t.Bounds.Min.Y))
	defer op.Affine(placement).Push(ops).Pop()
	defer clip.Rect{Max: image.Pt(t.Width, t.Height)}.Push(ops).Pop()
	c.images[index].Add(ops)
	paint.PaintOp{}.Add(ops)
}

type shapedRun struct {
	call  op.CallOp
	dims  layout.Dimensions
	shift f32.Point
	base  Baseline
}

func (c *Canvas) text(gtx layout.Context, chunk TextChunk) {
	label := gtx
	label.Metric = giounit.Metric{PxPerDp: 1, PxPerSp: 1}
	label.Constraints = layout.Constraints{Max: image.Pt(CoordinateLimit, CoordinateLimit)}
	runs := make([]shapedRun, 0, len(chunk.Runs))
	width := float32(0)
	for _, r := range chunk.Runs {
		s := c.shape(label, r)
		width += s.shift.X + float32(s.dims.Size.X)
		runs = append(runs, s)
	}
	pen := f32.Pt(chunk.Origin.X-anchorOffset(chunk.Anchor, width), chunk.Origin.Y)
	for _, s := range runs {
		pen = pen.Add(s.shift)
		top := pen.Y - baselineDrop(s.base, s.dims)
		stack := op.Affine(f32.Affine2D{}.Offset(f32.Pt(pen.X, top))).Push(gtx.Ops)
		s.call.Add(gtx.Ops)
		stack.Pop()
		pen.X += float32(s.dims.Size.X)
	}
}

func (c *Canvas) shape(gtx layout.Context, r TextRun) shapedRun {
	face := giofont.Font{Typeface: c.Faces.Sans, Weight: giofont.Weight(int(r.Weight) - 400)}
	if r.Face == FaceMono {
		face.Typeface = c.Faces.Mono
	}
	if r.Italic {
		face.Style = giofont.Italic
	}
	material := op.Record(gtx.Ops)
	paint.ColorOp{Color: nrgba(r.Fill.Color)}.Add(gtx.Ops)
	colour := material.Stop()
	macro := op.Record(gtx.Ops)
	dims := widget.Label{MaxLines: 1}.Layout(gtx, c.Shaper, face, giounit.Sp(r.Size), r.Text, colour)
	return shapedRun{call: macro.Stop(), dims: dims, shift: f32.Pt(r.Shift.X, r.Shift.Y), base: r.Baseline}
}

func anchorOffset(a Anchor, width float32) float32 {
	switch a {
	case AnchorMiddle:
		return width / 2
	case AnchorEnd:
		return width
	case AnchorStart:
	}
	return 0
}

func baselineDrop(b Baseline, d layout.Dimensions) float32 {
	ascent := float32(d.Size.Y - d.Baseline)
	switch b {
	case BaselineMiddle, BaselineCentral, BaselineMathematical:
		return float32(d.Size.Y) / 2
	case BaselineHanging, BaselineTextTop:
		return 0
	case BaselineTextBottom, BaselineIdeographic:
		return float32(d.Size.Y)
	case BaselineAlphabetic:
	}
	return ascent
}
