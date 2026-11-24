package figure

import (
	"math"
	"slices"
	"strings"
)

const (
	gradientBandsPerUnit = 64
	gradientBandLimit    = 1024
)

type maskInk struct{ r, g, b, a float64 }

func inkOf(c rgba) maskInk {
	return maskInk{c.r * c.a, c.g * c.a, c.b * c.a, c.a}
}

func (p maskInk) over(dst maskInk) maskInk {
	keep := 1 - p.a
	return maskInk{p.r + dst.r*keep, p.g + dst.g*keep, p.b + dst.b*keep, p.a + dst.a*keep}
}

func (p maskInk) scaled(k float64) maskInk {
	return maskInk{p.r * k, p.g * k, p.b * k, p.a * k}
}

type maskLayer struct {
	layer    int
	rule     fillRule
	color    rgba
	gradient *gradientDef
}

type maskElement struct {
	fill, stroke *maskLayer
	opacity      float64
}

const (
	coversFill byte = 1 << iota
	coversStroke
)

type maskCover struct {
	luminance bool
	gate      int
	elements  []maskElement
	tolerance float64
}

func (m *maskCover) classify(winding []int) (string, bool) {
	if winding[m.gate] == 0 {
		return "", false
	}
	pattern := make([]byte, len(m.elements))
	visible := false
	for i, e := range m.elements {
		if e.fill != nil && e.fill.rule.covers(winding[e.fill.layer]) {
			pattern[i] |= coversFill
		}
		if e.stroke != nil && e.stroke.rule.covers(winding[e.stroke.layer]) {
			pattern[i] |= coversStroke
		}
		visible = visible || pattern[i] != 0
	}
	return string(pattern), visible
}

type shade func(*maskLayer) rgba

func solidShade(l *maskLayer) rgba { return l.color }

func (m *maskCover) value(pattern string, colour shade) float64 {
	var acc maskInk
	for i, e := range m.elements {
		var own maskInk
		if pattern[i]&coversFill != 0 {
			own = inkOf(colour(e.fill))
		}
		if pattern[i]&coversStroke != 0 {
			own = inkOf(colour(e.stroke)).over(own)
		}
		acc = own.scaled(e.opacity).over(acc)
	}
	if m.luminance {
		return rgba{r: acc.r, g: acc.g, b: acc.b}.luminance()
	}
	return acc.a
}

func (m *maskCover) gradientIn(pattern string) ([]*maskLayer, bool) {
	var out []*maskLayer
	for i, e := range m.elements {
		if pattern[i]&coversFill != 0 && e.fill.gradient != nil {
			out = append(out, e.fill)
		}
		if pattern[i]&coversStroke != 0 && e.stroke.gradient != nil {
			out = append(out, e.stroke)
		}
	}
	return out, len(out) <= 1
}

func (m *maskCover) regions(areas []classArea) ([]coverRegion, bool) {
	constant := map[int]*shape{}
	var out []coverRegion
	for _, ca := range areas {
		layers, ok := m.gradientIn(ca.key)
		if !ok {
			return nil, false
		}
		if len(layers) == 1 {
			out = append(out, m.bands(ca, layers[0])...)
			continue
		}
		level := coverLevel(m.value(ca.key, solidShade))
		if level == 0 {
			continue
		}
		if constant[level] == nil {
			constant[level] = &shape{}
		}
		constant[level].append(ca.area)
	}
	levels := make([]int, 0, len(constant))
	for level := range constant {
		levels = append(levels, level)
	}
	slices.Sort(levels)
	for _, level := range levels {
		out = append(out, coverRegion{alpha: float64(level) / coverLevels, area: constant[level]})
	}
	return out, true
}

type coverRegion struct {
	alpha float64
	area  *shape
	band  *shape
}

const coverLevels = 255

func coverLevel(v float64) int { return int(math.Round(clamp01(v) * coverLevels)) }

type gradientBand struct {
	from, to float64
	level    int
}

func (m *maskCover) bands(ca classArea, layer *maskLayer) []coverRegion {
	g := layer.gradient
	inverse, ok := g.toUser.invert()
	lo, hi, _ := ca.area.bounds()
	if !ok {
		return nil
	}
	box := [2]vec{lo, hi}
	var merged []gradientBand
	stops := g.bandStops(box, inverse)
	for i := 0; i+1 < len(stops); i++ {
		t := (stops[i] + stops[i+1]) / 2
		level := coverLevel(m.value(ca.key, func(l *maskLayer) rgba {
			if l == layer {
				return g.colorAt(t)
			}
			return l.color
		}))
		if n := len(merged); n > 0 && merged[n-1].level == level {
			merged[n-1].to = stops[i+1]
			continue
		}
		merged = append(merged, gradientBand{from: stops[i], to: stops[i+1], level: level})
	}
	var out []coverRegion
	for _, b := range merged {
		if b.level == 0 {
			continue
		}
		band := g.bandShape(b, box, stops, m.tolerance)
		out = append(out, coverRegion{alpha: float64(b.level) / coverLevels, area: ca.area, band: band})
	}
	return out
}

func boxCorners(box [2]vec) [4]vec {
	return [4]vec{box[0], box[1], {box[0].x, box[1].y}, {box[1].x, box[0].y}}
}

func (g *gradientDef) bandStops(box [2]vec, inverse matrix) []float64 {
	lo, hi := math.Inf(1), math.Inf(-1)
	for _, c := range boxCorners(box) {
		if t, ok := g.offsetAt(inverse.vec(c)); ok {
			lo, hi = math.Min(lo, t), math.Max(hi, t)
		}
	}
	if g.radial {
		lo = 0
	}
	if lo > hi {
		return nil
	}
	stops := []float64{lo}
	first := math.Floor(lo*gradientBandsPerUnit) + 1
	for k := first; k/gradientBandsPerUnit < hi && k-first < gradientBandLimit; k++ {
		stops = append(stops, k/gradientBandsPerUnit)
	}
	return append(stops, hi)
}

func (g *gradientDef) offsetAt(q vec) (float64, bool) {
	if g.radial {
		return g.radialOffset(q)
	}
	axis := vec{g.x2 - g.x1, g.y2 - g.y1}
	length := axis.dot(axis)
	if length == 0 {
		return 0, false
	}
	return q.sub(vec{g.x1, g.y1}).dot(axis) / length, true
}

func (g *gradientDef) bandShape(b gradientBand, box [2]vec, stops []float64, tolerance float64) *shape {
	if g.radial {
		return g.annulus(b, box, stops, tolerance)
	}
	inverse, _ := g.toUser.invert()
	axis := vec{g.x2 - g.x1, g.y2 - g.y1}
	start := vec{g.x1, g.y1}
	lo, hi := math.Inf(1), math.Inf(-1)
	for _, c := range boxCorners(box) {
		s := inverse.vec(c).sub(start).dot(axis.perp()) / axis.dot(axis)
		lo, hi = math.Min(lo, s), math.Max(hi, s)
	}
	at := func(t, s float64) vec { return g.toUser.vec(start.add(axis.mul(t)).add(axis.perp().mul(s))) }
	out := &shape{}
	out.moveTo(at(b.from, lo))
	out.lineTo(at(b.to, lo))
	out.lineTo(at(b.to, hi))
	out.lineTo(at(b.from, hi))
	out.close()
	return out
}

func (g *gradientDef) annulus(b gradientBand, box [2]vec, stops []float64, tolerance float64) *shape {
	var out *shape
	if b.to >= stops[len(stops)-1] {
		out = rectShape(box[0], box[1].sub(box[0]))
	} else {
		out = orientedPositive(g.ring(b.to, tolerance))
	}
	if inner := g.ring(b.from, tolerance); b.from > 0 && !inner.empty() {
		out.append(orientedPositive(inner).reversedSubpaths())
	}
	return out
}

func (g *gradientDef) ring(t, tolerance float64) *shape {
	center := vec{g.fx, g.fy}.add(vec{g.cx - g.fx, g.cy - g.fy}.mul(t))
	radius := g.fr + t*(g.r-g.fr)
	out := &shape{}
	if radius <= 0 {
		return out
	}
	reach := radius * math.Sqrt(math.Abs(g.toUser.det()))
	steps := 8
	if reach > tolerance {
		steps = int(math.Ceil(math.Pi / math.Acos(1-tolerance/reach)))
	}
	steps = min(max(steps, 8), 256)
	for i := range steps {
		angle := 2 * math.Pi * float64(i) / float64(steps)
		p := g.toUser.vec(center.add(vec{math.Cos(angle), math.Sin(angle)}.mul(radius)))
		if i == 0 {
			out.moveTo(p)
			continue
		}
		out.lineTo(p)
	}
	out.close()
	return out
}

type maskSpace struct {
	state     renderState
	content   matrix
	tolerance float64
}

func (d *decoder) maskRegions(n *svgNode, id string, st renderState) ([]coverRegion, error) {
	mask, err := d.reference(n, id)
	if err != nil {
		return nil, err
	}
	if mask.name != "mask" {
		return nil, failAt(FailureInvalid, n.span, "mask url(#%s) names no mask", id)
	}
	region, contentSpace, err := d.maskRegion(n, mask, st)
	if err != nil {
		return nil, err
	}
	previous := d.override
	d.override = nil
	defer func() { d.override = previous }()
	space := maskSpace{state: st, content: contentSpace, tolerance: deviceTolerance / math.Max(d.resolution(st), 1e-6)}
	a := &arrangement{}
	cover := &maskCover{luminance: maskLuminance(mask), tolerance: space.tolerance}
	cover.gate = a.addShape(region, space.tolerance)
	if err := d.maskElements(a, cover, mask, space); err != nil {
		return nil, err
	}
	areas, ok := a.regions(cover.classify)
	if !ok {
		return nil, failAt(FailureLimit, mask.span, "mask needs more than %d edges or %d crossings; simplify its shapes", arrangeEdgeLimit, arrangeEventLimit)
	}
	regions, ok := cover.regions(areas)
	if !ok {
		return nil, failAt(FailureInvalid, mask.span, "mask shapes with gradients overlap; the supported mask profile shades each area with at most one gradient")
	}
	return regions, nil
}

func maskLuminance(mask *svgNode) bool {
	mt, _ := mask.attr("mask-type")
	return mt != "alpha"
}

func (d *decoder) maskElements(a *arrangement, cover *maskCover, mask *svgNode, space maskSpace) error {
	ms, err := d.domStyle(mask)
	if err != nil {
		return err
	}
	for _, c := range mask.children {
		if c.isText || c.name == "title" || c.name == "desc" {
			continue
		}
		element, visible, err := d.maskElement(a, c, ms, space)
		if err != nil {
			return err
		}
		if visible {
			cover.elements = append(cover.elements, element)
		}
	}
	return nil
}

func (d *decoder) maskElement(a *arrangement, c *svgNode, parent *style, space maskSpace) (maskElement, bool, error) {
	cs, err := d.maskStyle(c, parent)
	if err != nil || cs == nil {
		return maskElement{}, false, err
	}
	st := renderState{style: cs, ctm: space.state.ctm, viewport: space.state.viewport}
	geom, err := d.geometry(c, st)
	if err != nil || geom == nil {
		return maskElement{}, false, err
	}
	m, err := d.localTransform(c)
	if err != nil {
		return maskElement{}, false, err
	}
	element, err := d.maskLayers(a, c, geom, st, maskPlacement{toMask: space.content.mul(m), space: space})
	if err != nil {
		return maskElement{}, false, err
	}
	return element, element.fill != nil || element.stroke != nil, nil
}

func (d *decoder) maskStyle(c *svgNode, parent *style) (*style, error) {
	if !profileShapes[c.name] {
		return nil, failAt(FailureInvalid, c.span, "<%s> inside a mask is outside the supported mask profile; use shapes", c.name)
	}
	cs, err := d.computeStyle(c, parent)
	if err != nil || !cs.display || !cs.visible {
		return nil, err
	}
	if cs.clipPath != "" || cs.mask != "" || cs.filter != "" {
		return nil, failAt(FailureInvalid, c.span, "mask content with its own clip-path, mask, or filter is outside the supported mask profile")
	}
	return cs, nil
}

func (d *decoder) maskLayers(a *arrangement, c *svgNode, geom *shape, st renderState, at maskPlacement) (maskElement, error) {
	element := maskElement{opacity: st.style.opacity}
	var err error
	if c.name != "line" {
		if element.fill, err = d.maskFill(a, c, geom, st, at); err != nil {
			return maskElement{}, err
		}
	}
	if element.stroke, err = d.maskStroke(a, c, geom, st, at); err != nil {
		return maskElement{}, err
	}
	return element, nil
}

type maskPlacement struct {
	toMask matrix
	space  maskSpace
}

func (d *decoder) maskFill(a *arrangement, c *svgNode, geom *shape, st renderState, at maskPlacement) (*maskLayer, error) {
	paint, err := d.resolvePaint(c, st.style.fill, st.style.fillOpacity, geom, st)
	if err != nil || paint.none {
		return nil, err
	}
	return maskPaintLayer(a, maskSource{geom: geom.transformed(at.toMask), rule: st.style.fillRule, paint: paint}, at), nil
}

func (d *decoder) maskStroke(a *arrangement, c *svgNode, geom *shape, st renderState, at maskPlacement) (*maskLayer, error) {
	if st.style.stroke.kind == paintNone {
		return nil, nil
	}
	ss, err := d.strokeStyle(c, st)
	if err != nil || ss.width <= 0 {
		return nil, err
	}
	paint, err := d.resolvePaint(c, st.style.stroke, st.style.strokeOpacity, geom, st)
	if err != nil || paint.none {
		return nil, err
	}
	scale := math.Sqrt(math.Abs(at.toMask.det()))
	outline, ok := strokeOutline(geom, ss, at.space.tolerance/math.Max(scale, 1e-6))
	if !ok {
		return nil, failAt(FailureLimit, c.span, "dash pattern of <%s> produces more than %d dashes", c.name, dashStepLimit)
	}
	return maskPaintLayer(a, maskSource{geom: outline.transformed(at.toMask), rule: ruleNonZero, paint: paint}, at), nil
}

type maskSource struct {
	geom  *shape
	rule  fillRule
	paint resolvedPaint
}

func maskPaintLayer(a *arrangement, src maskSource, at maskPlacement) *maskLayer {
	layer := &maskLayer{layer: a.addShape(src.geom, at.space.tolerance), rule: src.rule, color: src.paint.color}
	if src.paint.gradient != nil {
		g := *src.paint.gradient
		g.toUser = at.toMask.mul(g.toUser)
		layer.gradient = &g
	}
	return layer
}

func (d *decoder) maskRegion(n, mask *svgNode, st renderState) (*shape, matrix, error) {
	units, _ := mask.attr("maskUnits")
	contentUnits, _ := mask.attr("maskContentUnits")
	var lo, hi vec
	if units != "userSpaceOnUse" || contentUnits == "objectBoundingBox" {
		var err error
		if lo, hi, err = d.objectBounds(n, st); err != nil {
			return nil, matrix{}, err
		}
	}
	contentSpace := identity
	if contentUnits == "objectBoundingBox" {
		contentSpace = boxMatrix(lo, hi)
	}
	if units == "userSpaceOnUse" {
		basis := d.basis(st, 0)
		x := d.lengthAttr(mask, "x", -0.1*st.viewport.x, basis, st.viewport.x)
		y := d.lengthAttr(mask, "y", -0.1*st.viewport.y, basis, st.viewport.y)
		w := d.lengthAttr(mask, "width", 1.2*st.viewport.x, basis, st.viewport.x)
		h := d.lengthAttr(mask, "height", 1.2*st.viewport.y, basis, st.viewport.y)
		return rectShape(vec{x, y}, vec{w, h}), contentSpace, nil
	}
	region := rectShape(vec{boxFraction(mask, "x", -0.1), boxFraction(mask, "y", -0.1)}, vec{boxFraction(mask, "width", 1.2), boxFraction(mask, "height", 1.2)})
	return region.transformed(boxMatrix(lo, hi)), contentSpace, nil
}

func boxFraction(mask *svgNode, name string, fallback float64) float64 {
	raw, ok := mask.attr(name)
	if !ok {
		return fallback
	}
	raw = strings.TrimSpace(raw)
	v, ok := fraction(raw, 1)
	if strings.HasSuffix(raw, "%") || !ok {
		return v
	}
	f, _ := parseLength(raw)
	return f.value
}

func (d *decoder) masked(regions []coverRegion, draw func() error) error {
	start := len(d.list.Ops)
	if err := draw(); err != nil {
		return err
	}
	content := slices.Clone(d.list.Ops[start:])
	d.list.Ops = d.list.Ops[:start]
	if len(content) == 0 {
		return nil
	}
	for _, r := range regions {
		pushes, err := d.pushRegion(r)
		if err != nil {
			return err
		}
		d.list.Ops = append(d.list.Ops, content...)
		d.pop(pushes)
		if len(d.list.Ops) > OperationLimit {
			return fail(FailureLimit, "masked content needs more than %d display operations", OperationLimit)
		}
	}
	return nil
}

func (d *decoder) pushRegion(r coverRegion) (int, error) {
	if err := d.pushClip(r.area); err != nil {
		return 0, err
	}
	pushes := 1
	if r.band != nil {
		if err := d.pushClip(r.band); err != nil {
			return pushes, err
		}
		pushes++
	}
	if r.alpha < 1 {
		d.list.Ops = append(d.list.Ops, Op{Kind: OpOpacity, Alpha: float32(r.alpha)})
		pushes++
	}
	return pushes, nil
}
