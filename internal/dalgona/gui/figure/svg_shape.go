package figure

import (
	"math"
	"strings"
)

func (d *decoder) geometry(n *svgNode, st renderState) (*shape, error) {
	basis := d.basis(st, 0)
	vw, vh, vd := st.viewport.x, st.viewport.y, diagonal(st.viewport)
	num := func(name string, percent float64) float64 { return d.lengthAttr(n, name, 0, basis, percent) }
	var s *shape
	switch n.name {
	case "rect":
		s = rectGeometry(num("x", vw), num("y", vh), num("width", vw), num("height", vh), d.radii(n, basis, st.viewport))
	case "circle":
		r := num("r", vd)
		s = ellipseGeometry(vec{num("cx", vw), num("cy", vh)}, r, r)
	case "ellipse":
		s = ellipseGeometry(vec{num("cx", vw), num("cy", vh)}, num("rx", vw), num("ry", vh))
	case "line":
		s = &shape{}
		s.moveTo(vec{num("x1", vw), num("y1", vh)})
		s.lineTo(vec{num("x2", vw), num("y2", vh)})
	default:
		return d.pathGeometry(n)
	}
	if s == nil {
		return nil, nil
	}
	return s, checkShape(n, s)
}

func rectGeometry(x, y, w, h float64, r vec) *shape {
	if !(w > 0 && h > 0) {
		return nil
	}
	return roundedRectShape(vec{x, y}, vec{w, h}, vec{math.Min(r.x, w/2), math.Min(r.y, h/2)})
}

func ellipseGeometry(c vec, rx, ry float64) *shape {
	if !(rx > 0 && ry > 0) {
		return nil
	}
	return ellipseShape(c, rx, ry)
}

func (d *decoder) radii(n *svgNode, basis lengthBasis, viewport vec) vec {
	rx := d.lengthAttr(n, "rx", -1, basis, viewport.x)
	ry := d.lengthAttr(n, "ry", -1, basis, viewport.y)
	switch {
	case rx < 0 && ry < 0:
		return vec{}
	case rx < 0:
		rx = ry
	case ry < 0:
		ry = rx
	}
	return vec{rx, ry}
}

func (d *decoder) pathGeometry(n *svgNode) (*shape, error) {
	switch n.name {
	case "path":
		raw, _ := n.attr("d")
		s, problem := parsePathData(raw)
		if problem != "" {
			return nil, failAt(FailureInvalid, n.span, "%s", problem)
		}
		return s, checkShape(n, s)
	case "polyline", "polygon":
		raw, _ := n.attr("points")
		pts, ok := parsePoints(raw)
		if !ok {
			return nil, failAt(FailureInvalid, n.span, "points of <%s> must be coordinate pairs within the coordinate limit", n.name)
		}
		if len(pts) == 0 {
			return nil, nil
		}
		s := &shape{}
		s.moveTo(pts[0])
		for _, p := range pts[1:] {
			s.lineTo(p)
		}
		if n.name == "polygon" {
			s.close()
		}
		return s, nil
	}
	return nil, failAt(FailureInvalid, n.span, "<%s> is not a drawable element", n.name)
}

func checkShape(n *svgNode, s *shape) error {
	for _, p := range s.pts {
		if !p.within() {
			return failAt(FailureLimit, n.span, "<%s> has a coordinate beyond %d logical pixels or not finite", n.name, CoordinateLimit)
		}
	}
	return nil
}

func (d *decoder) shapeElement(n *svgNode, st renderState) error {
	geom, err := d.geometry(n, st)
	if err != nil || geom == nil || geom.empty() {
		return err
	}
	if err := d.checkRootSpace(n, geom, st); err != nil {
		return err
	}
	if st.style.visible {
		if err := d.paintFill(n, geom, st); err != nil {
			return err
		}
		if err := d.paintStroke(n, geom, st); err != nil {
			return err
		}
	}
	switch n.name {
	case "path", "line", "polyline", "polygon":
		return d.markers(n, geom, st)
	}
	return nil
}

func (d *decoder) checkRootSpace(n *svgNode, geom *shape, st renderState) error {
	lo, hi, ok := geom.bounds()
	if !ok {
		return nil
	}
	for _, corner := range [...]vec{lo, hi, {lo.x, hi.y}, {hi.x, lo.y}} {
		if !st.ctm.vec(corner).within() {
			return failAt(FailureLimit, n.span, "<%s> lands beyond %d logical pixels after its transforms", n.name, CoordinateLimit)
		}
	}
	return nil
}

func (d *decoder) resolution(st renderState) float64 {
	return d.opts.Scale * math.Sqrt(math.Abs(st.ctm.det()))
}

func (d *decoder) paintFill(n *svgNode, geom *shape, st renderState) error {
	if n.name == "line" {
		return nil
	}
	paint, err := d.resolvePaint(n, st.style.fill, st.style.fillOpacity, geom, st)
	if err != nil || paint.none {
		return err
	}
	return d.emitFill(n, geom, st.style.fillRule, paint, st)
}

func (d *decoder) emitFill(n *svgNode, geom *shape, rule fillRule, paint resolvedPaint, st renderState) error {
	if rule == ruleEvenOdd || (paint.gradient != nil && paint.gradient.radial) {
		return d.rasterFill(n, geom, rule, paint, st)
	}
	p := SolidPaint(paint.color.color())
	if paint.gradient != nil {
		gradient, err := d.linearGradient(paint.gradient)
		if err != nil {
			return err
		}
		p.Gradient = gradient
	}
	d.list.Paths = append(d.list.Paths, geom.path())
	index, err := listIndex(len(d.list.Paths) - 1)
	if err != nil {
		return err
	}
	d.list.Ops = append(d.list.Ops, Op{Kind: OpFill, Index: index, Paint: p})
	return nil
}

func (d *decoder) rasterFill(n *svgNode, geom *shape, rule fillRule, paint resolvedPaint, st renderState) error {
	res := d.resolution(st)
	if res <= 0 {
		return nil
	}
	r, ok := newRaster(geom, rule, res)
	if !ok {
		return failAt(FailureLimit, n.span, "<%s> needs more than %d raster pixels for its fill rule or gradient", n.name, rasterPixelLimit)
	}
	fill := func(vec) rgba { return paint.color }
	if paint.gradient != nil {
		gradientPaint, ok := paint.gradient.rasterPaint()
		if !ok {
			return nil
		}
		fill = gradientPaint
	}
	for _, t := range r.render(fill) {
		d.rasterBytes += len(t.Pix)
		if d.rasterBytes > rasterByteBudget {
			return failAt(FailureLimit, n.span, "figure raster tiles exceed %d bytes", rasterByteBudget)
		}
		d.list.Tiles = append(d.list.Tiles, t)
		index, err := listIndex(len(d.list.Tiles) - 1)
		if err != nil {
			return err
		}
		d.list.Ops = append(d.list.Ops, Op{Kind: OpImage, Index: index})
	}
	return nil
}

func (d *decoder) linearGradient(g *gradientDef) (int32, error) {
	stops := make([]Stop, 0, len(g.stops)*gradientSubdivision)
	for i, s := range g.stops {
		if i > 0 {
			prev := g.stops[i-1]
			for k := 1; k < gradientSubdivision; k++ {
				t := float64(k) / gradientSubdivision
				offset := prev.offset + (s.offset-prev.offset)*t
				stops = append(stops, Stop{Offset: float32(offset), Color: mixPremultiplied(prev.color, s.color, 1-t, t).color()})
			}
		}
		stops = append(stops, Stop{Offset: float32(s.offset), Color: s.color.color()})
	}
	d.list.Gradients = append(d.list.Gradients, Gradient{
		Start:  vec{g.x1, g.y1}.point(),
		End:    vec{g.x2, g.y2}.point(),
		Stops:  stops,
		Spread: g.spread,
		Space:  g.toUser.affine(),
	})
	return listIndex(len(d.list.Gradients) - 1)
}

const gradientSubdivision = 4

func (d *decoder) strokeStyle(n *svgNode, st renderState) (strokeStyle, error) {
	basis := d.basis(st, diagonal(st.viewport))
	s := strokeStyle{
		width:      st.style.strokeWidth.resolve(basis),
		cap:        st.style.lineCap,
		join:       st.style.lineJoin,
		miterLimit: st.style.miterLimit,
		dashOffset: st.style.dashOffset.resolve(basis),
	}
	total := 0.0
	for _, l := range st.style.dashes {
		v := l.resolve(basis)
		s.dashes = append(s.dashes, v)
		total += v
	}
	if total <= 0 {
		s.dashes = nil
	}
	if len(s.dashes)%2 == 1 {
		s.dashes = append(s.dashes, s.dashes...)
	}
	if !withinLimit(s.width) || !withinLimit(s.dashOffset) {
		return s, failAt(FailureLimit, n.span, "stroke of <%s> exceeds the coordinate limit", n.name)
	}
	return s, nil
}

func (d *decoder) paintStroke(n *svgNode, geom *shape, st renderState) error {
	if st.style.stroke.kind == paintNone {
		return nil
	}
	ss, err := d.strokeStyle(n, st)
	if err != nil || ss.width <= 0 {
		return err
	}
	paint, err := d.resolvePaint(n, st.style.stroke, st.style.strokeOpacity, geom, st)
	if err != nil || paint.none {
		return err
	}
	res := d.resolution(st)
	if res <= 0 {
		return nil
	}
	outline, err := strokedOutline(n, geom, ss, deviceTolerance/res)
	if err != nil || outline.empty() {
		return err
	}
	return d.emitFill(n, outline, ruleNonZero, paint, st)
}

func strokedOutline(n *svgNode, geom *shape, ss strokeStyle, tolerance float64) (*shape, error) {
	outline, ok := strokeOutline(geom, ss, tolerance)
	if !ok {
		return nil, failAt(FailureLimit, n.span, "dash pattern of <%s> produces more than %d dashes", n.name, dashStepLimit)
	}
	if len(outline.verbs) > OperationLimit {
		return nil, failAt(FailureLimit, n.span, "stroke of <%s> needs more than %d display operations", n.name, OperationLimit)
	}
	return outline, nil
}

type resolvedPaint struct {
	none     bool
	color    rgba
	gradient *gradientDef
}

func (d *decoder) resolvePaint(n *svgNode, spec paintSpec, opacity float64, geom *shape, st renderState) (resolvedPaint, error) {
	switch spec.kind {
	case paintNone:
		return resolvedPaint{none: true}, nil
	case paintCurrent:
		return d.overridden(resolvedPaint{color: st.style.color.withAlpha(opacity)}), nil
	case paintColor:
		return d.overridden(resolvedPaint{color: spec.color.withAlpha(opacity)}), nil
	case paintURL:
	}
	target, ok := d.ids[spec.ref]
	if !ok || (target.name != "linearGradient" && target.name != "radialGradient") {
		if spec.fallback != nil {
			return d.resolvePaint(n, *spec.fallback, opacity, geom, st)
		}
		return resolvedPaint{}, failAt(FailureInvalid, n.span, "paint url(#%s) on <%s> names no gradient; define it or use a color", spec.ref, n.name)
	}
	return d.gradientPaint(target, opacity, geom, st)
}

func (d *decoder) gradientPaint(target *svgNode, opacity float64, geom *shape, st renderState) (resolvedPaint, error) {
	g, err := d.gradient(target, geom, st)
	if err != nil || g == nil {
		return resolvedPaint{none: g == nil}, err
	}
	for i := range g.stops {
		g.stops[i].color = g.stops[i].color.withAlpha(opacity)
	}
	if len(g.stops) == 1 {
		return d.overridden(resolvedPaint{color: g.stops[0].color}), nil
	}
	if d.override != nil {
		for i := range g.stops {
			g.stops[i].color = rgba{r: d.override.r, g: d.override.g, b: d.override.b, a: g.stops[i].color.a}
		}
	}
	return resolvedPaint{gradient: g}, nil
}

func (d *decoder) overridden(p resolvedPaint) resolvedPaint {
	if d.override != nil {
		p.color = rgba{r: d.override.r, g: d.override.g, b: d.override.b, a: p.color.a}
	}
	return p
}

func (d *decoder) gradient(n *svgNode, geom *shape, st renderState) (*gradientDef, error) {
	chain, err := d.gradientChain(n)
	if err != nil {
		return nil, err
	}
	attr := func(name string) (string, bool) {
		for _, g := range chain {
			if v, ok := g.attr(name); ok {
				return v, true
			}
		}
		return "", false
	}
	g := &gradientDef{radial: n.name == "radialGradient"}
	stops, err := d.gradientStops(chain)
	if err != nil || len(stops) == 0 {
		return nil, err
	}
	g.stops = stops
	units, _ := attr("gradientUnits")
	space, err := d.gradientSpace(n, attr, units != "userSpaceOnUse", geom)
	if err != nil || space == nil {
		return nil, err
	}
	g.toUser = *space
	g.spread = map[string]Spread{"reflect": SpreadReflect, "repeat": SpreadRepeat}[firstOf(attr("spreadMethod"))]
	return g, d.gradientGeometry(n, g, attr, units != "userSpaceOnUse", st)
}

func firstOf(v string, _ bool) string { return v }

func (d *decoder) gradientChain(n *svgNode) ([]*svgNode, error) {
	chain := []*svgNode{n}
	for len(chain) <= 16 {
		raw, ok := chain[len(chain)-1].href()
		if !ok {
			return chain, nil
		}
		next, found := d.ids[strings.TrimPrefix(strings.TrimSpace(raw), "#")]
		if !found || (next.name != "linearGradient" && next.name != "radialGradient") {
			return nil, failAt(FailureInvalid, n.span, "gradient refers to %s, which is not a gradient", quoted(raw))
		}
		for _, seen := range chain {
			if seen == next {
				return nil, failAt(FailureInvalid, n.span, "gradient references form a cycle; break the cycle")
			}
		}
		chain = append(chain, next)
	}
	return nil, failAt(FailureLimit, n.span, "gradient references chain more than 16 gradients")
}

func (d *decoder) gradientStops(chain []*svgNode) ([]stopDef, error) {
	for _, g := range chain {
		var stops []stopDef
		for _, c := range g.children {
			if c.name != "stop" {
				continue
			}
			s, err := d.stop(c, stops)
			if err != nil {
				return nil, err
			}
			stops = append(stops, s)
		}
		if len(stops) > 64 {
			return nil, failAt(FailureLimit, g.span, "gradient has more than 64 stops")
		}
		if len(stops) > 0 {
			return stops, nil
		}
	}
	return nil, nil
}

func (d *decoder) stop(n *svgNode, previous []stopDef) (stopDef, error) {
	parent, err := d.domStyle(n.parent)
	if err != nil {
		return stopDef{}, err
	}
	st, err := d.computeStyle(n, parent)
	if err != nil {
		return stopDef{}, err
	}
	raw, _ := n.attr("offset")
	offset, ok := fraction(strings.TrimSpace(raw), 1)
	if raw == "" {
		offset, ok = 0, true
	}
	if !ok {
		return stopDef{}, failAt(FailureInvalid, n.span, "stop offset %s is malformed", quoted(raw))
	}
	if len(previous) > 0 {
		offset = math.Max(offset, previous[len(previous)-1].offset)
	}
	c := st.stopColor.color
	if st.stopColor.kind == paintCurrent {
		c = st.color
	}
	return stopDef{offset: offset, color: c.withAlpha(st.stopOpacity)}, nil
}

func (d *decoder) gradientSpace(n *svgNode, attr func(string) (string, bool), bbox bool, geom *shape) (*matrix, error) {
	m := identity
	if raw, ok := attr("gradientTransform"); ok {
		t, ok := parseTransform(raw)
		if !ok {
			return nil, failAt(FailureInvalid, n.span, "gradientTransform %s is malformed or not finite", quoted(raw))
		}
		m = t
	}
	if !bbox {
		return &m, nil
	}
	lo, hi, ok := geom.bounds()
	if !ok || hi.x-lo.x <= 0 || hi.y-lo.y <= 0 {
		return nil, nil
	}
	space := matrix{a: hi.x - lo.x, d: hi.y - lo.y, e: lo.x, f: lo.y}.mul(m)
	return &space, nil
}

func (d *decoder) gradientGeometry(n *svgNode, g *gradientDef, attr func(string) (string, bool), bbox bool, st renderState) error {
	value := func(name string, fallback float64, percent float64) (float64, error) {
		raw, ok := attr(name)
		if !ok {
			return fallback, nil
		}
		v, ok := gradientLength(raw, bbox, d.basis(st, percent))
		if !ok {
			return 0, failAt(FailureInvalid, n.span, "gradient %s %s is malformed", name, quoted(raw))
		}
		return v, nil
	}
	w, h, dg := st.viewport.x, st.viewport.y, diagonal(st.viewport)
	if bbox {
		w, h, dg = 1, 1, 1
	}
	var err error
	coords := []struct {
		target   *float64
		name     string
		fallback float64
		percent  float64
	}{
		{&g.x1, "x1", 0, w},
		{&g.y1, "y1", 0, h},
		{&g.x2, "x2", w, w},
		{&g.y2, "y2", 0, h},
		{&g.cx, "cx", w / 2, w},
		{&g.cy, "cy", h / 2, h},
		{&g.r, "r", dg / 2, dg},
		{&g.fr, "fr", 0, dg},
	}
	for _, c := range coords {
		if *c.target, err = value(c.name, c.fallback, c.percent); err != nil {
			return err
		}
	}
	if g.fx, err = value("fx", g.cx, w); err != nil {
		return err
	}
	g.fy, err = value("fy", g.cy, h)
	return err
}

func gradientLength(raw string, bbox bool, basis lengthBasis) (float64, bool) {
	l, ok := parseLength(raw)
	if !ok {
		return 0, false
	}
	if bbox && l.unit == unitPercent {
		return l.value / 100, true
	}
	if bbox {
		return l.value, true
	}
	return l.resolve(basis), true
}
