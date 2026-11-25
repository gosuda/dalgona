package figure

import (
	"math"
	"strings"
)

func (d *decoder) domStyle(n *svgNode) (*style, error) {
	if n == nil {
		return initialStyle(), nil
	}
	if s, ok := d.domStyles[n]; ok {
		return s, nil
	}
	parent, err := d.domStyle(n.parent)
	if err != nil {
		return nil, err
	}
	s, err := d.computeStyle(n, parent)
	if err != nil {
		return nil, err
	}
	d.domStyles[n] = s
	return s, nil
}

type cutout struct {
	clips  []*shape
	mask   []coverRegion
	masked bool
}

func (c cutout) hidden() bool {
	for _, s := range c.clips {
		if s.empty() {
			return true
		}
	}
	return false
}

func (d *decoder) cutout(n *svgNode, st renderState) (cutout, error) {
	var out cutout
	if st.style.clipPath != "" {
		clips, err := d.clipPathShapes(n, st.style.clipPath, st, 0)
		if err != nil {
			return out, err
		}
		out.clips = clips
	}
	if st.style.mask != "" {
		regions, err := d.maskRegions(n, st.style.mask, st)
		if err != nil {
			return out, err
		}
		out.mask, out.masked = regions, true
	}
	return out, nil
}

func (d *decoder) objectBounds(n *svgNode, st renderState) (vec, vec, error) {
	geom, err := d.boundsGeometry(n, st, 0)
	if err != nil {
		return vec{}, vec{}, err
	}
	lo, hi, ok := geom.bounds()
	if !ok {
		return vec{}, vec{}, nil
	}
	return lo, hi, nil
}

func (d *decoder) boundsGeometry(n *svgNode, st renderState, depth int) (*shape, error) {
	if depth > SVGDepthLimit {
		return nil, failAt(FailureLimit, n.span, "bounding box nests deeper than %d levels", SVGDepthLimit)
	}
	switch n.name {
	case "g", "a":
		return d.groupBounds(n, st, depth)
	case "text", "use", "svg":
		return nil, failAt(FailureInvalid, n.span, "objectBoundingBox units on <%s> are outside the static SVG profile; use userSpaceOnUse", n.name)
	}
	geom, err := d.geometry(n, st)
	if geom == nil {
		geom = &shape{}
	}
	return geom, err
}

func (d *decoder) groupBounds(n *svgNode, st renderState, depth int) (*shape, error) {
	out := &shape{}
	for _, c := range n.children {
		if c.isText || definitionElements[c.name] {
			continue
		}
		child, err := d.boundsGeometry(c, st, depth+1)
		if err != nil {
			return nil, err
		}
		m, err := d.localTransform(c)
		if err != nil {
			return nil, err
		}
		out.append(child.transformed(m))
	}
	return out, nil
}

func boxMatrix(lo, hi vec) matrix {
	return matrix{a: hi.x - lo.x, d: hi.y - lo.y, e: lo.x, f: lo.y}
}

const clipNestingLimit = 8

func (d *decoder) clipPathShapes(n *svgNode, id string, st renderState, depth int) ([]*shape, error) {
	clip, err := d.reference(n, id)
	if err != nil {
		return nil, err
	}
	if clip.name != "clipPath" || depth > clipNestingLimit {
		return nil, failAt(FailureInvalid, n.span, "clip-path url(#%s) names no clipPath or nests too deeply", id)
	}
	m, err := d.clipSpace(n, clip, st)
	if err != nil {
		return nil, err
	}
	d.references = append(d.references, clip)
	defer func() { d.references = d.references[:len(d.references)-1] }()
	tolerance := deviceTolerance / math.Max(d.resolution(st)*math.Sqrt(math.Abs(m.det())), 1e-6)
	union, err := d.clipUnion(clip, st, tolerance)
	if err != nil {
		return nil, err
	}
	nested, err := d.nestedClip(clip, st, depth)
	out := []*shape{union.transformed(m)}
	for _, s := range nested {
		out = append(out, s.transformed(m))
	}
	return out, err
}

func (d *decoder) clipSpace(n, clip *svgNode, st renderState) (matrix, error) {
	m, err := d.localTransform(clip)
	if err != nil {
		return matrix{}, err
	}
	if units, _ := clip.attr("clipPathUnits"); units != "objectBoundingBox" {
		return m, nil
	}
	lo, hi, err := d.objectBounds(n, st)
	if err != nil {
		return matrix{}, err
	}
	return boxMatrix(lo, hi).mul(m), nil
}

func (d *decoder) nestedClip(clip *svgNode, st renderState, depth int) ([]*shape, error) {
	cs, err := d.domStyle(clip)
	if err != nil || cs.clipPath == "" {
		return nil, err
	}
	return d.clipPathShapes(clip, cs.clipPath, st, depth+1)
}

func (d *decoder) clipUnion(clip *svgNode, st renderState, tolerance float64) (*shape, error) {
	parent, err := d.domStyle(clip)
	if err != nil {
		return nil, err
	}
	union := &shape{}
	for _, c := range clip.children {
		if c.isText || c.name == "title" || c.name == "desc" {
			continue
		}
		part, err := d.clipChild(c, parent, st)
		if err != nil {
			return nil, err
		}
		if part.geom == nil {
			continue
		}
		area, ok := part.area(tolerance)
		if !ok {
			return nil, failAt(FailureLimit, c.span, "clip child needs more than %d edges or %d crossings; simplify it", arrangeEdgeLimit, arrangeEventLimit)
		}
		union.append(area)
	}
	return union, nil
}

type clipPart struct {
	geom *shape
	rule fillRule
}

func (p clipPart) area(tolerance float64) (*shape, bool) {
	if p.rule != ruleEvenOdd && subpaths(p.geom) == 1 {
		return orientedPositive(p.geom), true
	}
	return solidArea(p.geom, p.rule, tolerance)
}

func subpaths(s *shape) int {
	count := 0
	for _, v := range s.verbs {
		if v == VerbMove {
			count++
		}
	}
	return count
}

func (d *decoder) clipChild(c *svgNode, parent *style, st renderState) (clipPart, error) {
	target, placement, err := d.clipTarget(c, st)
	if err != nil {
		return clipPart{}, err
	}
	cs, err := d.computeStyle(target, parent)
	if err != nil || !cs.display || !cs.visible {
		return clipPart{}, err
	}
	if cs.clipPath != "" {
		return clipPart{}, failAt(FailureInvalid, c.span, "nested clip-path on clip children is outside the static SVG profile")
	}
	geom, err := d.geometry(target, renderState{style: cs, ctm: st.ctm, viewport: st.viewport})
	if err != nil || geom == nil {
		return clipPart{}, err
	}
	m, err := d.localTransform(target)
	if err != nil {
		return clipPart{}, err
	}
	return clipPart{geom: geom.transformed(placement.mul(m)), rule: cs.clipRule}, nil
}

func (d *decoder) clipTarget(c *svgNode, st renderState) (*svgNode, matrix, error) {
	target, placement := c, identity
	if c.name == "use" {
		raw, _ := c.href()
		ref, err := d.reference(c, strings.TrimPrefix(strings.TrimSpace(raw), "#"))
		if err != nil {
			return nil, matrix{}, err
		}
		useTransform, err := d.localTransform(c)
		if err != nil {
			return nil, matrix{}, err
		}
		basis := d.basis(st, 0)
		offset := translate(d.lengthAttr(c, "x", 0, basis, st.viewport.x), d.lengthAttr(c, "y", 0, basis, st.viewport.y))
		target, placement = ref, useTransform.mul(offset)
	}
	if !profileShapes[target.name] {
		return nil, matrix{}, failAt(FailureInvalid, c.span, "<%s> inside a clipPath is outside the static SVG profile; use shapes", target.name)
	}
	return target, placement, nil
}

var profileShapes = map[string]bool{
	"path": true, "rect": true, "circle": true, "ellipse": true, "line": true, "polyline": true, "polygon": true,
}

func orientedPositive(s *shape) *shape {
	if signedArea(s.flatten(1e-2)) < 0 {
		return s.reversedSubpaths()
	}
	return s
}
