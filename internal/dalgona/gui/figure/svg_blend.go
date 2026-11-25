package figure

import (
	"math"
	"slices"
)

type blendSource struct {
	fill  Op
	space matrix
	alpha float64
}

type blendFrame struct {
	space matrix
	alpha float64
}

func (d *decoder) multiply(n *svgNode, start int) error {
	src, found, err := d.blendSource(n, start)
	if err != nil || !found {
		return err
	}
	frame := openFrame(d.list.Ops[:start])
	if !drawsAny(d.list.Ops[frame.isolation:start]) {
		return nil
	}
	inside, err := d.blendArea(n, src, frame)
	if err != nil {
		return err
	}
	factor := multiplyFactor(src.fill.Paint.Color, src.alpha)
	backdrop := slices.Clone(d.list.Ops[frame.isolation:start])
	tinted, err := d.tinted(n, backdrop, factor)
	if err != nil {
		return err
	}
	ops := slices.Concat(d.list.Ops[:frame.isolation], frame.pushes, d.list.Ops[start:])
	ops = appendPops(ops, len(frame.pushes))
	if ops, err = d.appendClipped(ops, inside, tinted, len(frame.pushes)); err != nil {
		return err
	}
	if ops, err = d.appendClipped(ops, outsideOf(inside), backdrop, len(frame.pushes)); err != nil {
		return err
	}
	ops = append(ops, frame.pushes...)
	d.list.Ops = ops
	if len(d.list.Ops) > OperationLimit {
		return failAt(FailureLimit, n.span, "multiply blending of <%s> needs more than %d display operations", n.name, OperationLimit)
	}
	return nil
}

func (d *decoder) blendArea(n *svgNode, src blendSource, frame isolationFrame) (*shape, error) {
	tolerance := deviceTolerance / math.Max(d.opts.Scale, 1e-6)
	region := shapeOf(d.list.Paths[src.fill.Index]).transformed(frame.space.mul(src.space))
	if !frame.contains(d.list.Paths, region) {
		return nil, failAt(FailureInvalid, n.span, "multiply blending of <%s> crosses a clip edge; the profile blends shapes that lie inside their clips", n.name)
	}
	inside, ok := solidArea(region, ruleNonZero, tolerance)
	if !ok {
		return nil, failAt(FailureLimit, n.span, "multiply blending of <%s> needs more than %d edges; simplify the shape", n.name, arrangeEdgeLimit)
	}
	return inside, nil
}

func (d *decoder) blendSource(n *svgNode, start int) (blendSource, bool, error) {
	refuse := failAt(FailureInvalid, n.span, "multiply blending applies to one solid-filled shape; <%s> draws other content", n.name)
	stack := []blendFrame{{space: identity, alpha: 1}}
	var src blendSource
	found := false
	for _, o := range d.list.Ops[start:] {
		top := stack[len(stack)-1]
		switch o.Kind {
		case OpTransform:
			stack = append(stack, blendFrame{space: top.space.mul(matrixOf(o.Transform)), alpha: top.alpha})
		case OpOpacity:
			stack = append(stack, blendFrame{space: top.space, alpha: top.alpha * float64(o.Alpha)})
		case OpPop:
			stack = stack[:max(len(stack)-1, 1)]
		case OpFill:
			if found || o.Paint.Gradient >= 0 {
				return src, false, refuse
			}
			src, found = blendSource{fill: o, space: top.space, alpha: top.alpha}, true
		case OpClip, OpText, OpImage:
			return src, false, refuse
		}
	}
	return src, found, nil
}

func matrixOf(a Affine) matrix {
	return matrix{float64(a.A), float64(a.B), float64(a.C), float64(a.D), float64(a.E), float64(a.F)}
}

func shapeOf(p Path) *shape {
	s := &shape{}
	k := 0
	at := func(i int) vec { return vec{float64(p.Points[k+i].X), float64(p.Points[k+i].Y)} }
	for _, v := range p.Verbs {
		switch v {
		case VerbMove:
			s.moveTo(at(0))
		case VerbLine:
			s.lineTo(at(0))
		case VerbQuad:
			s.quadTo(at(0), at(1))
		case VerbCube:
			s.cubeTo(at(0), at(1), at(2), true)
		case VerbClose:
			s.close()
		}
		k += verbPoints[v]
	}
	return s
}

func drawsAny(ops []Op) bool {
	return slices.ContainsFunc(ops, func(o Op) bool { return o.Kind == OpFill || o.Kind == OpText || o.Kind == OpImage })
}

type isolationFrame struct {
	isolation int
	pushes    []Op
	space     matrix
}

func openFrame(ops []Op) isolationFrame {
	var open []int
	for i, o := range ops {
		switch o.Kind {
		case OpTransform, OpClip, OpOpacity:
			open = append(open, i)
		case OpPop:
			open = open[:max(len(open)-1, 0)]
		case OpFill, OpText, OpImage:
		}
	}
	first := 0
	frame := isolationFrame{space: identity}
	for k, i := range open {
		if ops[i].Kind == OpOpacity {
			first, frame.isolation = k+1, i+1
		}
	}
	for _, i := range open[first:] {
		frame.pushes = append(frame.pushes, ops[i])
		if ops[i].Kind == OpTransform {
			frame.space = frame.space.mul(matrixOf(ops[i].Transform))
		}
	}
	return frame
}

func (f isolationFrame) contains(paths []Path, region *shape) bool {
	lo, hi, ok := region.bounds()
	if !ok {
		return true
	}
	space := identity
	for _, o := range f.pushes {
		if o.Kind == OpTransform {
			space = space.mul(matrixOf(o.Transform))
			continue
		}
		if !convexContains(shapeOf(paths[o.Index]).transformed(space), lo, hi) {
			return false
		}
	}
	return true
}

func convexContains(clip *shape, lo, hi vec) bool {
	lines := clip.flatten(1e-3)
	if len(lines) != 1 || len(lines[0].points) < 3 {
		return false
	}
	pts := lines[0].points
	sign := 0.0
	for i, p := range pts {
		edge := pts[(i+1)%len(pts)].sub(p)
		for _, corner := range [...]vec{lo, hi, {lo.x, hi.y}, {hi.x, lo.y}} {
			side := edge.cross(corner.sub(p))
			if side*sign < -1e-9 {
				return false
			}
			if sign == 0 && math.Abs(side) > 1e-9 {
				sign = side
			}
		}
	}
	return true
}

func multiplyFactor(c Color, alpha float64) [3]float64 {
	a := alpha * float64(c.A) / 255
	var f [3]float64
	for i, v := range [...]uint8{c.R, c.G, c.B} {
		f[i] = 1 - a*(1-float64(v)/255)
	}
	return f
}

func tint(c Color, f [3]float64) Color {
	return Color{R: channel(float64(c.R) / 255 * f[0]), G: channel(float64(c.G) / 255 * f[1]), B: channel(float64(c.B) / 255 * f[2]), A: c.A}
}

func (d *decoder) tinted(n *svgNode, ops []Op, f [3]float64) ([]Op, error) {
	out := slices.Clone(ops)
	for i, o := range out {
		var err error
		switch o.Kind {
		case OpFill:
			out[i].Paint, err = d.tintPaint(o.Paint, f)
		case OpText:
			out[i].Index, err = d.tintChunk(o.Index, f)
		case OpImage:
			out[i].Index, err = d.tintTile(n, o.Index, f)
		case OpTransform, OpClip, OpOpacity, OpPop:
		}
		if err != nil {
			return nil, err
		}
	}
	return out, nil
}

func (d *decoder) tintPaint(p Paint, f [3]float64) (Paint, error) {
	if p.Gradient < 0 {
		p.Color = tint(p.Color, f)
		return p, nil
	}
	g := d.list.Gradients[p.Gradient]
	g.Stops = slices.Clone(g.Stops)
	for i := range g.Stops {
		g.Stops[i].Color = tint(g.Stops[i].Color, f)
	}
	d.list.Gradients = append(d.list.Gradients, g)
	index, err := listIndex(len(d.list.Gradients) - 1)
	if err != nil {
		return Paint{}, err
	}
	p.Gradient = index
	return p, nil
}

func (d *decoder) tintChunk(index int32, f [3]float64) (int32, error) {
	c := d.list.Chunks[index]
	c.Runs = slices.Clone(c.Runs)
	for i := range c.Runs {
		fill, err := d.tintPaint(c.Runs[i].Fill, f)
		if err != nil {
			return 0, err
		}
		c.Runs[i].Fill = fill
	}
	d.list.Chunks = append(d.list.Chunks, c)
	return listIndex(len(d.list.Chunks) - 1)
}

func (d *decoder) tintTile(n *svgNode, index int32, f [3]float64) (int32, error) {
	t := d.list.Tiles[index]
	d.rasterBytes += len(t.Pix)
	if d.rasterBytes > rasterByteBudget {
		return 0, failAt(FailureLimit, n.span, "multiply blending of <%s> needs raster tiles beyond %d bytes", n.name, rasterByteBudget)
	}
	t.Pix = slices.Clone(t.Pix)
	for i := 0; i+3 < len(t.Pix); i += 4 {
		for c := range 3 {
			t.Pix[i+c] = channel(float64(t.Pix[i+c]) / 255 * f[c])
		}
	}
	d.list.Tiles = append(d.list.Tiles, t)
	return listIndex(len(d.list.Tiles) - 1)
}

func (d *decoder) appendClipped(ops []Op, area *shape, content []Op, open int) ([]Op, error) {
	d.list.Paths = append(d.list.Paths, area.path())
	index, err := listIndex(len(d.list.Paths) - 1)
	if err != nil {
		return nil, err
	}
	ops = append(ops, Op{Kind: OpClip, Index: index})
	ops = append(ops, content...)
	return appendPops(ops, open+1), nil
}

func appendPops(ops []Op, n int) []Op {
	for range n {
		ops = append(ops, Op{Kind: OpPop})
	}
	return ops
}

func outsideOf(area *shape) *shape {
	out := rectShape(vec{-CoordinateLimit, -CoordinateLimit}, vec{2 * CoordinateLimit, 2 * CoordinateLimit})
	out.append(area.reversedSubpaths())
	return out
}
