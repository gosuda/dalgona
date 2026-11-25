package figure

import (
	"math"
)

type vec struct{ x, y float64 }

func (v vec) add(w vec) vec        { return vec{v.x + w.x, v.y + w.y} }
func (v vec) sub(w vec) vec        { return vec{v.x - w.x, v.y - w.y} }
func (v vec) mul(s float64) vec    { return vec{v.x * s, v.y * s} }
func (v vec) dot(w vec) float64    { return v.x*w.x + v.y*w.y }
func (v vec) cross(w vec) float64  { return v.x*w.y - v.y*w.x }
func (v vec) length() float64      { return math.Hypot(v.x, v.y) }
func (v vec) perp() vec            { return vec{-v.y, v.x} }
func (m matrix) vec(v vec) vec     { x, y := m.apply(v.x, v.y); return vec{x, y} }
func (v vec) point() Point         { return Point{X: float32(v.x), Y: float32(v.y)} }
func (v vec) within() bool         { return withinLimit(v.x) && withinLimit(v.y) }
func (v vec) unit() vec            { return v.mul(1 / v.length()) }
func lerp(a, b vec, t float64) vec { return a.add(b.sub(a).mul(t)) }
func (v vec) near(w vec) bool      { return v.sub(w).length() < 1e-9 }
func (v vec) rotated(angle float64) vec {
	s, c := math.Sincos(angle)
	return vec{v.x*c - v.y*s, v.x*s + v.y*c}
}

type shape struct {
	verbs  []Verb
	pts    []vec
	vertex []bool
	start  vec
	pen    vec
	open   bool
}

func (p *shape) moveTo(v vec) {
	p.verbs, p.pts, p.vertex = append(p.verbs, VerbMove), append(p.pts, v), append(p.vertex, true)
	p.start, p.pen, p.open = v, v, true
}

func (p *shape) ensureOpen() {
	if !p.open {
		p.moveTo(p.pen)
	}
}

func (p *shape) lineTo(v vec) {
	p.ensureOpen()
	p.verbs, p.pts, p.vertex = append(p.verbs, VerbLine), append(p.pts, v), append(p.vertex, true)
	p.pen = v
}

func (p *shape) quadTo(c, v vec) {
	p.ensureOpen()
	p.verbs, p.pts, p.vertex = append(p.verbs, VerbQuad), append(p.pts, c, v), append(p.vertex, true)
	p.pen = v
}

func (p *shape) cubeTo(c1, c2, v vec, vertex bool) {
	p.ensureOpen()
	p.verbs, p.pts, p.vertex = append(p.verbs, VerbCube), append(p.pts, c1, c2, v), append(p.vertex, vertex)
	p.pen = v
}

func (p *shape) close() {
	if !p.open {
		return
	}
	p.verbs, p.vertex = append(p.verbs, VerbClose), append(p.vertex, true)
	p.pen, p.open = p.start, false
}

func (p *shape) empty() bool { return len(p.verbs) == 0 }

func (p *shape) transformed(m matrix) *shape {
	out := &shape{verbs: p.verbs, vertex: p.vertex, pts: make([]vec, len(p.pts))}
	for i, v := range p.pts {
		out.pts[i] = m.vec(v)
	}
	return out
}

func (p *shape) reversedSubpaths() *shape {
	out := &shape{}
	walkSubpaths(p, func(sub segmentRun) {
		if len(sub.segments) == 0 {
			return
		}
		out.moveTo(sub.segments[len(sub.segments)-1].end())
		for i := len(sub.segments) - 1; i >= 0; i-- {
			out.appendReversed(sub.segments[i])
		}
		if sub.closed {
			out.close()
		}
	})
	return out
}

func (p *shape) appendReversed(s segment) {
	switch s.verb {
	case VerbQuad:
		p.quadTo(s.p[1], s.p[0])
	case VerbCube:
		p.cubeTo(s.p[2], s.p[1], s.p[0], true)
	case VerbMove, VerbLine, VerbClose:
		p.lineTo(s.p[0])
	}
}

func (p *shape) path() Path {
	out := Path{Verbs: p.verbs, Points: make([]Point, len(p.pts))}
	for i, v := range p.pts {
		out.Points[i] = v.point()
	}
	return out
}

func (p *shape) append(q *shape) {
	p.verbs = append(p.verbs, q.verbs...)
	p.pts = append(p.pts, q.pts...)
	p.vertex = append(p.vertex, q.vertex...)
	p.open = false
}

type segment struct {
	verb   Verb
	p      [4]vec
	vertex bool
}

func (s segment) end() vec {
	switch s.verb {
	case VerbQuad:
		return s.p[2]
	case VerbCube:
		return s.p[3]
	case VerbMove, VerbLine, VerbClose:
	}
	return s.p[1]
}

func (s segment) startTangent() vec {
	for i := 1; i <= s.degree(); i++ {
		if d := s.p[i].sub(s.p[0]); d.length() > 1e-12 {
			return d
		}
	}
	return vec{}
}

func (s segment) endTangent() vec {
	n := s.degree()
	for i := n - 1; i >= 0; i-- {
		if d := s.p[n].sub(s.p[i]); d.length() > 1e-12 {
			return d
		}
	}
	return vec{}
}

func (s segment) degree() int {
	switch s.verb {
	case VerbQuad:
		return 2
	case VerbCube:
		return 3
	case VerbMove, VerbLine, VerbClose:
	}
	return 1
}

type segmentRun struct {
	start    vec
	segments []segment
	closed   bool
}

func walkSubpaths(p *shape, visit func(segmentRun)) {
	var run segmentRun
	pen, k := vec{}, 0
	flush := func() {
		if run.segments != nil || run.closed {
			visit(run)
		}
	}
	for i, v := range p.verbs {
		switch v {
		case VerbMove:
			flush()
			pen = p.pts[k]
			run = segmentRun{start: pen}
		case VerbLine, VerbQuad, VerbCube:
			seg := segment{verb: v, vertex: p.vertex[i]}
			seg.p[0] = pen
			copy(seg.p[1:], p.pts[k:k+verbPoints[v]])
			run.segments = append(run.segments, seg)
		case VerbClose:
			if !pen.near(run.start) {
				run.segments = append(run.segments, segment{verb: VerbLine, p: [4]vec{pen, run.start}, vertex: true})
			}
			run.closed = true
			flush()
			run = segmentRun{start: run.start}
			pen = run.start
			continue
		}
		k += verbPoints[v]
		if v != VerbMove {
			pen = run.segments[len(run.segments)-1].end()
		}
	}
	flush()
}

const kappa = 0.5522847498307936

func ellipseShape(c vec, rx, ry float64) *shape {
	p := &shape{}
	p.moveTo(vec{c.x + rx, c.y})
	quarter := [4][3]vec{
		{{c.x + rx, c.y + ry*kappa}, {c.x + rx*kappa, c.y + ry}, {c.x, c.y + ry}},
		{{c.x - rx*kappa, c.y + ry}, {c.x - rx, c.y + ry*kappa}, {c.x - rx, c.y}},
		{{c.x - rx, c.y - ry*kappa}, {c.x - rx*kappa, c.y - ry}, {c.x, c.y - ry}},
		{{c.x + rx*kappa, c.y - ry}, {c.x + rx, c.y - ry*kappa}, {c.x + rx, c.y}},
	}
	for _, q := range quarter {
		p.cubeTo(q[0], q[1], q[2], true)
	}
	p.close()
	return p
}

func rectShape(origin, size vec) *shape {
	x, y, w, h := origin.x, origin.y, size.x, size.y
	p := &shape{}
	p.moveTo(vec{x, y})
	p.lineTo(vec{x + w, y})
	p.lineTo(vec{x + w, y + h})
	p.lineTo(vec{x, y + h})
	p.close()
	return p
}

func roundedRectShape(origin, size, radius vec) *shape {
	if radius.x <= 0 || radius.y <= 0 {
		return rectShape(origin, size)
	}
	x, y, w, h, rx, ry := origin.x, origin.y, size.x, size.y, radius.x, radius.y
	p := &shape{}
	kx, ky := rx*(1-kappa), ry*(1-kappa)
	p.moveTo(vec{x + rx, y})
	p.lineTo(vec{x + w - rx, y})
	p.cubeTo(vec{x + w - kx, y}, vec{x + w, y + ky}, vec{x + w, y + ry}, true)
	p.lineTo(vec{x + w, y + h - ry})
	p.cubeTo(vec{x + w, y + h - ky}, vec{x + w - kx, y + h}, vec{x + w - rx, y + h}, true)
	p.lineTo(vec{x + rx, y + h})
	p.cubeTo(vec{x + kx, y + h}, vec{x, y + h - ky}, vec{x, y + h - ry}, true)
	p.lineTo(vec{x, y + ry})
	p.cubeTo(vec{x, y + ky}, vec{x + kx, y}, vec{x + rx, y}, true)
	p.close()
	return p
}

func (p *shape) arcTo(r vec, rotation float64, large, sweep bool, to vec) {
	from := p.pen
	if from.near(to) {
		return
	}
	rx, ry := math.Abs(r.x), math.Abs(r.y)
	if rx == 0 || ry == 0 {
		p.lineTo(to)
		return
	}
	phi := rotation * math.Pi / 180
	mid := from.sub(to).mul(0.5).rotated(-phi)
	if lambda := mid.x*mid.x/(rx*rx) + mid.y*mid.y/(ry*ry); lambda > 1 {
		s := math.Sqrt(lambda)
		rx, ry = rx*s, ry*s
	}
	num := rx*rx*ry*ry - rx*rx*mid.y*mid.y - ry*ry*mid.x*mid.x
	den := rx*rx*mid.y*mid.y + ry*ry*mid.x*mid.x
	coef := math.Sqrt(math.Max(0, num/den))
	if large == sweep {
		coef = -coef
	}
	centerPrime := vec{coef * rx * mid.y / ry, -coef * ry * mid.x / rx}
	center := centerPrime.rotated(phi).add(from.add(to).mul(0.5))
	u := vec{(mid.x - centerPrime.x) / rx, (mid.y - centerPrime.y) / ry}
	v := vec{(-mid.x - centerPrime.x) / rx, (-mid.y - centerPrime.y) / ry}
	theta := math.Atan2(u.y, u.x)
	delta := arcSweep(u, v, sweep)
	p.arcSegments(ellipseArc{center: center, rx: rx, ry: ry, phi: phi}, theta, delta, to)
}

func arcSweep(u, v vec, sweep bool) float64 {
	delta := math.Atan2(u.cross(v), u.dot(v))
	if !sweep && delta > 0 {
		return delta - 2*math.Pi
	}
	if sweep && delta < 0 {
		return delta + 2*math.Pi
	}
	return delta
}

type ellipseArc struct {
	center vec
	rx, ry float64
	phi    float64
}

func (e ellipseArc) at(t float64) vec {
	s, c := math.Sincos(t)
	return vec{e.rx * c, e.ry * s}.rotated(e.phi).add(e.center)
}

func (e ellipseArc) derivative(t float64) vec {
	s, c := math.Sincos(t)
	return vec{-e.rx * s, e.ry * c}.rotated(e.phi)
}

func (p *shape) arcSegments(e ellipseArc, theta, delta float64, to vec) {
	n := int(math.Ceil(math.Abs(delta) / (math.Pi / 2)))
	n = max(n, 1)
	step := delta / float64(n)
	alpha := 4.0 / 3 * math.Tan(step/4)
	for i := range n {
		t0, t1 := theta+float64(i)*step, theta+float64(i+1)*step
		end := e.at(t1)
		if i == n-1 {
			end = to
		}
		c1 := e.at(t0).add(e.derivative(t0).mul(alpha))
		c2 := end.sub(e.derivative(t1).mul(alpha))
		p.cubeTo(c1, c2, end, i == n-1)
	}
}

func (p *shape) bounds() (vec, vec, bool) {
	lo, hi := vec{math.Inf(1), math.Inf(1)}, vec{math.Inf(-1), math.Inf(-1)}
	include := func(v vec) {
		lo = vec{math.Min(lo.x, v.x), math.Min(lo.y, v.y)}
		hi = vec{math.Max(hi.x, v.x), math.Max(hi.y, v.y)}
	}
	walkSubpaths(p, func(run segmentRun) {
		include(run.start)
		for _, s := range run.segments {
			for _, v := range s.extrema() {
				include(v)
			}
		}
	})
	return lo, hi, lo.x <= hi.x
}

func (s segment) extrema() []vec {
	out := []vec{s.end()}
	for _, t := range s.criticalTimes() {
		if t > 0 && t < 1 {
			out = append(out, s.at(t))
		}
	}
	return out
}

func (s segment) at(t float64) vec {
	switch s.verb {
	case VerbQuad:
		return lerp(lerp(s.p[0], s.p[1], t), lerp(s.p[1], s.p[2], t), t)
	case VerbCube:
		a, b, c := lerp(s.p[0], s.p[1], t), lerp(s.p[1], s.p[2], t), lerp(s.p[2], s.p[3], t)
		return lerp(lerp(a, b, t), lerp(b, c, t), t)
	case VerbMove, VerbLine, VerbClose:
	}
	return lerp(s.p[0], s.p[1], t)
}

func (s segment) criticalTimes() []float64 {
	switch s.verb {
	case VerbQuad:
		return append(quadCritical(s.p[0].x, s.p[1].x, s.p[2].x), quadCritical(s.p[0].y, s.p[1].y, s.p[2].y)...)
	case VerbCube:
		return append(cubeCritical(s.p[0].x, s.p[1].x, s.p[2].x, s.p[3].x), cubeCritical(s.p[0].y, s.p[1].y, s.p[2].y, s.p[3].y)...)
	case VerbMove, VerbLine, VerbClose:
	}
	return nil
}

func quadCritical(a, b, c float64) []float64 {
	den := a - 2*b + c
	if den == 0 {
		return nil
	}
	return []float64{(a - b) / den}
}

func cubeCritical(p0, p1, p2, p3 float64) []float64 {
	a := -p0 + 3*p1 - 3*p2 + p3
	b := 2 * (p0 - 2*p1 + p2)
	c := p1 - p0
	if math.Abs(a) < 1e-12 {
		if b == 0 {
			return nil
		}
		return []float64{-c / b}
	}
	disc := b*b - 4*a*c
	if disc < 0 {
		return nil
	}
	sq := math.Sqrt(disc)
	return []float64{(-b + sq) / (2 * a), (-b - sq) / (2 * a)}
}

func (s segment) flatten(tolerance float64, out []vec) []vec {
	n := s.pieces(tolerance)
	for i := 1; i <= n; i++ {
		out = append(out, s.at(float64(i)/float64(n)))
	}
	return out
}

const flattenPieceLimit = 1024

func (s segment) pieces(tolerance float64) int {
	var dd float64
	switch s.verb {
	case VerbQuad:
		dd = 0.25 * s.p[0].sub(s.p[1].mul(2)).add(s.p[2]).length()
	case VerbCube:
		d1 := s.p[0].sub(s.p[1].mul(2)).add(s.p[2]).length()
		d2 := s.p[1].sub(s.p[2].mul(2)).add(s.p[3]).length()
		dd = 0.75 * math.Max(d1, d2)
	case VerbMove, VerbLine, VerbClose:
		return 1
	}
	n := math.Ceil(math.Sqrt(dd / tolerance))
	if math.IsNaN(n) || n < 1 {
		return 1
	}
	return int(math.Min(n, flattenPieceLimit))
}

type polyline struct {
	points []vec
	closed bool
}

func (p *shape) flatten(tolerance float64) []polyline {
	var out []polyline
	walkSubpaths(p, func(run segmentRun) {
		pts := []vec{run.start}
		for _, s := range run.segments {
			pts = s.flatten(tolerance, pts)
		}
		out = append(out, polyline{points: pts, closed: run.closed})
	})
	return out
}

func signedArea(lines []polyline) float64 {
	var area float64
	for _, l := range lines {
		for i := range l.points {
			a, b := l.points[i], l.points[(i+1)%len(l.points)]
			area += a.cross(b)
		}
	}
	return area / 2
}
