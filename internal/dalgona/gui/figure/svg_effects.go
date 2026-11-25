package figure

import (
	"math"
	"strconv"
	"strings"
)

type filterKind uint8

const (
	filterSource filterKind = iota
	filterAlpha
	filterFlood
	filterSilhouette
	filterBlur
	filterStack
)

type filterLayer struct {
	source bool
	color  rgba
	offset vec
}

type filterValue struct {
	kind   filterKind
	color  rgba
	offset vec
	layers []filterLayer
}

func (v filterValue) stack() ([]filterLayer, bool) {
	switch v.kind {
	case filterSource:
		return []filterLayer{{source: true, offset: v.offset}}, true
	case filterSilhouette:
		return []filterLayer{{color: v.color, offset: v.offset}}, true
	case filterStack:
		return v.layers, true
	case filterAlpha, filterFlood, filterBlur:
	}
	return nil, false
}

func (d *decoder) filtered(n *svgNode, st renderState, body func(renderState) error) error {
	if st.style.filter == "" {
		return body(st)
	}
	layers, err := d.filterLayers(n, st.style.filter)
	if err != nil {
		return err
	}
	for _, layer := range layers {
		if err := d.filterLayer(n, st, layer, body); err != nil {
			return err
		}
	}
	return nil
}

func (d *decoder) filterLayer(n *svgNode, st renderState, layer filterLayer, body func(renderState) error) error {
	pushes := 0
	if layer.offset != (vec{}) {
		if !d.pushTransform(translate(layer.offset.x, layer.offset.y), &st) {
			return failAt(FailureLimit, n.span, "filter offset exceeds the coordinate limit")
		}
		pushes++
	}
	previous := d.override
	if !layer.source {
		if layer.color.a < 1 {
			d.list.Ops = append(d.list.Ops, Op{Kind: OpOpacity, Alpha: float32(layer.color.a)})
			pushes++
		}
		flood := layer.color
		d.override = &flood
	}
	err := body(st)
	d.override = previous
	d.pop(pushes)
	return err
}

func (d *decoder) filterLayers(n *svgNode, id string) ([]filterLayer, error) {
	f, err := d.reference(n, id)
	if err != nil {
		return nil, err
	}
	if f.name != "filter" {
		return nil, failAt(FailureInvalid, n.span, "filter url(#%s) names no filter", id)
	}
	results := map[string]filterValue{}
	last := filterValue{kind: filterSource}
	for _, p := range f.children {
		if p.isText {
			continue
		}
		v, err := d.primitive(p, results, last)
		if err != nil {
			return nil, err
		}
		if name, ok := p.attr("result"); ok {
			results[name] = v
		}
		last = v
	}
	layers, ok := last.stack()
	if !ok {
		return nil, failAt(FailureInvalid, f.span, "filter output is not a source graphic or offset silhouette; the engine effect profile cannot draw it")
	}
	return layers, nil
}

func filterInput(p *svgNode, attr string, results map[string]filterValue, last filterValue) (filterValue, error) {
	name, ok := p.attr(attr)
	if !ok {
		return last, nil
	}
	switch name {
	case "SourceGraphic":
		return filterValue{kind: filterSource}, nil
	case "SourceAlpha":
		return filterValue{kind: filterAlpha}, nil
	}
	v, ok := results[name]
	if !ok {
		return filterValue{}, failAt(FailureInvalid, p.span, "filter input %s is not produced by an earlier primitive", quoted(name))
	}
	if v.kind == filterBlur {
		return filterValue{}, failAt(FailureInvalid, p.span, "blurred filter output is outside the engine effect profile")
	}
	return v, nil
}

func (d *decoder) primitive(p *svgNode, results map[string]filterValue, last filterValue) (filterValue, error) {
	in, err := filterInput(p, "in", results, last)
	if err != nil {
		return filterValue{}, err
	}
	switch p.name {
	case "feGaussianBlur":
		return filterValue{kind: filterBlur}, nil
	case "feFlood":
		return d.flood(p)
	case "feOffset":
		return offsetPrimitive(p, in)
	case "feComposite":
		return compositePrimitive(p, in, results, last)
	case "feBlend":
		return blendPrimitive(p, in, results, last)
	case "feMerge":
		return mergePrimitive(p, results, last)
	}
	return filterValue{}, failAt(FailureInvalid, p.span, "<%s> is outside the engine effect profile", p.name)
}

func (d *decoder) flood(p *svgNode) (filterValue, error) {
	st, err := d.domStyle(p)
	if err != nil {
		return filterValue{}, err
	}
	c := st.floodColor.color
	if st.floodColor.kind == paintCurrent {
		c = st.color
	}
	return filterValue{kind: filterFlood, color: c.withAlpha(st.floodOpacity)}, nil
}

func offsetPrimitive(p *svgNode, in filterValue) (filterValue, error) {
	dx, okX := numberAttr(p, "dx")
	dy, okY := numberAttr(p, "dy")
	if !okX || !okY || (in.kind != filterSilhouette && in.kind != filterSource) {
		return filterValue{}, failAt(FailureInvalid, p.span, "feOffset needs numeric dx and dy over a silhouette or source graphic")
	}
	in.offset = in.offset.add(vec{dx, dy})
	return in, nil
}

func numberAttr(n *svgNode, name string) (float64, bool) {
	raw, ok := n.attr(name)
	if !ok {
		return 0, true
	}
	v, err := strconv.ParseFloat(strings.TrimSpace(raw), 64)
	return v, err == nil && withinLimit(v)
}

func compositePrimitive(p *svgNode, in filterValue, results map[string]filterValue, last filterValue) (filterValue, error) {
	under, err := filterInput(p, "in2", results, last)
	if err != nil {
		return filterValue{}, err
	}
	operator, _ := p.attr("operator")
	if operator == "in" && in.kind == filterFlood && (under.kind == filterAlpha || under.kind == filterSource) && under.offset == (vec{}) {
		return filterValue{kind: filterSilhouette, color: in.color}, nil
	}
	if operator == "" || operator == "over" {
		return stackOf(p, under, in)
	}
	return filterValue{}, failAt(FailureInvalid, p.span, "feComposite operator %s over these inputs is outside the engine effect profile", quoted(operator))
}

func blendPrimitive(p *svgNode, in filterValue, results map[string]filterValue, last filterValue) (filterValue, error) {
	under, err := filterInput(p, "in2", results, last)
	if err != nil {
		return filterValue{}, err
	}
	if mode, _ := p.attr("mode"); mode != "" && mode != "normal" {
		return filterValue{}, failAt(FailureInvalid, p.span, "feBlend mode %s is outside the engine effect profile", mode)
	}
	return stackOf(p, under, in)
}

func stackOf(p *svgNode, values ...filterValue) (filterValue, error) {
	out := filterValue{kind: filterStack}
	for _, v := range values {
		layers, ok := v.stack()
		if !ok {
			return filterValue{}, failAt(FailureInvalid, p.span, "<%s> input is not drawable in the engine effect profile", p.name)
		}
		out.layers = append(out.layers, layers...)
	}
	return out, nil
}

func mergePrimitive(p *svgNode, results map[string]filterValue, last filterValue) (filterValue, error) {
	var values []filterValue
	for _, c := range p.children {
		if c.isText {
			continue
		}
		if c.name != "feMergeNode" {
			return filterValue{}, failAt(FailureInvalid, c.span, "<%s> inside feMerge is outside the engine effect profile", c.name)
		}
		v, err := filterInput(c, "in", results, last)
		if err != nil {
			return filterValue{}, err
		}
		values = append(values, v)
	}
	return stackOf(p, values...)
}

type markerVertex struct {
	at    vec
	in    vec
	out   vec
	place int
}

func markerVertices(geom *shape) []markerVertex {
	var out []markerVertex
	walkSubpaths(geom, func(run segmentRun) {
		first := len(out)
		out = append(out, markerVertex{at: run.start, place: 1})
		for i, s := range run.segments {
			if i == 0 || out[len(out)-1].out == (vec{}) {
				out[len(out)-1].out = s.startTangent()
			}
			if s.vertex {
				out = append(out, markerVertex{at: s.end(), in: s.endTangent(), place: 1})
			}
		}
		if run.closed && len(run.segments) > 0 {
			out[first].in = run.segments[len(run.segments)-1].endTangent()
			out[len(out)-1].out = run.segments[0].startTangent()
		}
	})
	if len(out) > 0 {
		out[0].place = 0
		out[len(out)-1].place = 2
	}
	return out
}

func (v markerVertex) angle() float64 {
	in, out := v.in, v.out
	switch {
	case in == (vec{}) && out == (vec{}):
		return 0
	case in == (vec{}):
		return degrees(out)
	case out == (vec{}):
		return degrees(in)
	}
	a, b := degrees(in), degrees(out)
	if math.Abs(b-a) > 180 {
		b -= math.Copysign(360, b-a)
	}
	return (a + b) / 2
}

func degrees(v vec) float64 { return math.Atan2(v.y, v.x) * 180 / math.Pi }

func (d *decoder) markers(n *svgNode, geom *shape, st renderState) error {
	if st.style.markers == [3]string{} {
		return nil
	}
	for _, v := range markerVertices(geom) {
		id := st.style.markers[v.place]
		if id == "" {
			continue
		}
		if err := d.marker(n, id, v, st); err != nil {
			return err
		}
	}
	return nil
}

func (d *decoder) markerAngle(m *svgNode, v markerVertex) (float64, error) {
	orient, _ := m.attr("orient")
	switch strings.TrimSpace(orient) {
	case "auto":
		return v.angle(), nil
	case "auto-start-reverse":
		if v.place == 0 {
			return v.angle() + 180, nil
		}
		return v.angle(), nil
	case "":
		return 0, nil
	}
	a, err := strconv.ParseFloat(strings.TrimSuffix(strings.TrimSpace(orient), "deg"), 64)
	if err != nil || math.IsInf(a, 0) {
		return 0, failAt(FailureInvalid, m.span, "marker orient %s is malformed", quoted(orient))
	}
	return a, nil
}

func (d *decoder) marker(n *svgNode, id string, v markerVertex, st renderState) error {
	m, err := d.reference(n, id)
	if err != nil {
		return err
	}
	if m.name != "marker" {
		return failAt(FailureInvalid, n.span, "marker url(#%s) names no marker", id)
	}
	ms, err := d.domStyle(m)
	if err != nil {
		return err
	}
	angle, err := d.markerAngle(m, v)
	if err != nil {
		return err
	}
	basis := d.basis(st, 0)
	size := vec{d.lengthAttr(m, "markerWidth", 3, basis, st.viewport.x), d.lengthAttr(m, "markerHeight", 3, basis, st.viewport.y)}
	if !(size.x > 0 && size.y > 0) {
		return nil
	}
	unitScale := 1.0
	if units, _ := m.attr("markerUnits"); units != "userSpaceOnUse" {
		unitScale = st.style.strokeWidth.resolve(d.basis(st, diagonal(st.viewport)))
	}
	content, err := markerContent(m, size)
	if err != nil {
		return err
	}
	ref := content.vec(vec{d.lengthAttr(m, "refX", 0, basis, size.x), d.lengthAttr(m, "refY", 0, basis, size.y)})
	place := translate(v.at.x, v.at.y).mul(rotate(angle)).mul(scale(unitScale, unitScale)).mul(translate(-ref.x, -ref.y))
	d.references = append(d.references, m)
	err = d.markerBody(m, ms, markerViewport{place: place, content: content, size: size}, st)
	d.references = d.references[:len(d.references)-1]
	return err
}

func markerContent(m *svgNode, size vec) (matrix, error) {
	vb, has, err := viewBoxOf(m)
	if err != nil || !has {
		return identity, err
	}
	return aspectTransform(m, vb, size)
}

type markerViewport struct {
	place   matrix
	content matrix
	size    vec
}

func (d *decoder) markerBody(m *svgNode, ms *style, view markerViewport, st renderState) error {
	inner := renderState{style: ms, ctm: st.ctm, viewport: view.size, depth: st.depth + 1}
	if !d.pushTransform(view.place, &inner) {
		return failAt(FailureLimit, m.span, "marker placement exceeds the coordinate limit")
	}
	pushes := 1
	if !ms.overflow {
		if err := d.pushClip(rectShape(vec{0, 0}, view.size)); err != nil {
			d.pop(pushes)
			return err
		}
		pushes++
	}
	if !view.content.isIdentity() {
		if !d.pushTransform(view.content, &inner) {
			d.pop(pushes)
			return failAt(FailureLimit, m.span, "marker viewBox exceeds the coordinate limit")
		}
		pushes++
	}
	err := d.children(m, inner)
	d.pop(pushes)
	return err
}
