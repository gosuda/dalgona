package figure

import (
	"math"
)

type strokeStyle struct {
	width      float64
	cap        lineCap
	join       lineJoin
	miterLimit float64
	dashes     []float64
	dashOffset float64
}

type stroker struct {
	style strokeStyle
	half  float64
	out   *shape
}

func strokeOutline(geom *shape, st strokeStyle, tolerance float64) (*shape, bool) {
	s := &stroker{style: st, half: st.width / 2, out: &shape{}}
	for _, line := range geom.flatten(tolerance) {
		pts := dedupe(line.points)
		if len(st.dashes) == 0 {
			s.polyline(pts, line.closed)
			continue
		}
		dashes, ok := dashPolylines(pts, line.closed, st.dashes, st.dashOffset)
		if !ok {
			return nil, false
		}
		for _, dash := range dashes {
			s.polyline(dedupe(dash), false)
		}
	}
	return s.out, true
}

func dedupe(pts []vec) []vec {
	out := pts[:0:0]
	for i, p := range pts {
		if i == 0 || !p.near(out[len(out)-1]) {
			out = append(out, p)
		}
	}
	return out
}

func (s *stroker) polyline(pts []vec, closed bool) {
	if closed && len(pts) > 1 && pts[0].near(pts[len(pts)-1]) {
		pts = pts[:len(pts)-1]
	}
	switch len(pts) {
	case 0:
		return
	case 1:
		s.dot(pts[0])
		return
	}
	if closed {
		s.ring(pts)
		return
	}
	n := len(pts)
	for i := range n - 1 {
		s.quad(pts[i], pts[i+1])
	}
	for i := 1; i < n-1; i++ {
		s.joint(pts[i-1], pts[i], pts[i+1])
	}
	s.cap(pts[0], pts[0].sub(pts[1]))
	s.cap(pts[n-1], pts[n-1].sub(pts[n-2]))
}

func (s *stroker) ring(pts []vec) {
	n := len(pts)
	for i := range n {
		s.quad(pts[i], pts[(i+1)%n])
		s.joint(pts[(i+n-1)%n], pts[i], pts[(i+1)%n])
	}
}

func (s *stroker) polygon(pts ...vec) {
	area := 0.0
	for i := range pts {
		area += pts[i].cross(pts[(i+1)%len(pts)])
	}
	if area == 0 {
		return
	}
	if area < 0 {
		for i, j := 0, len(pts)-1; i < j; i, j = i+1, j-1 {
			pts[i], pts[j] = pts[j], pts[i]
		}
	}
	s.out.moveTo(pts[0])
	for _, p := range pts[1:] {
		s.out.lineTo(p)
	}
	s.out.close()
}

func (s *stroker) circle(c vec) {
	s.out.append(ellipseShape(c, s.half, s.half))
}

func (s *stroker) quad(a, b vec) {
	n := b.sub(a).unit().perp().mul(s.half)
	s.polygon(a.add(n), b.add(n), b.sub(n), a.sub(n))
}

func (s *stroker) dot(p vec) {
	switch s.style.cap {
	case capRound:
		s.circle(p)
	case capSquare:
		h := s.half
		s.polygon(vec{p.x - h, p.y - h}, vec{p.x + h, p.y - h}, vec{p.x + h, p.y + h}, vec{p.x - h, p.y + h})
	case capButt:
	}
}

func (s *stroker) cap(end, outward vec) {
	d := outward.unit().mul(s.half)
	n := d.perp()
	switch s.style.cap {
	case capRound:
		s.circle(end)
	case capSquare:
		s.polygon(end.add(n), end.add(n).add(d), end.sub(n).add(d), end.sub(n))
	case capButt:
	}
}

func (s *stroker) joint(prev, at, next vec) {
	in, out := at.sub(prev).unit(), next.sub(at).unit()
	turn := in.cross(out)
	if math.Abs(turn) < 1e-12 && in.dot(out) > 0 {
		return
	}
	if s.style.join == joinRound {
		s.circle(at)
		return
	}
	side := 1.0
	if turn > 0 {
		side = -1
	}
	a := at.add(in.perp().mul(s.half * side))
	b := at.add(out.perp().mul(s.half * side))
	s.polygon(at, a, b)
	if s.style.join == joinMiter {
		s.miter(at, a, b, in, out)
	}
}

func (s *stroker) miter(at, a, b, in, out vec) {
	cosTheta := -in.dot(out)
	ratio := 1 / math.Sqrt(math.Max((1-cosTheta)/2, 1e-24))
	if ratio > s.style.miterLimit {
		return
	}
	denominator := in.cross(out)
	if math.Abs(denominator) < 1e-12 {
		return
	}
	t := b.sub(a).cross(out) / denominator
	tip := a.add(in.mul(t))
	s.polygon(a, tip, b, at)
}

const dashStepLimit = 4 * OperationLimit

func dashPolylines(pts []vec, closed bool, pattern []float64, offset float64) ([][]vec, bool) {
	if closed && len(pts) > 1 {
		pts = append(pts[:len(pts):len(pts)], pts[0])
	}
	total := 0.0
	for _, d := range pattern {
		total += d
	}
	if total <= 0 || len(pts) < 2 {
		return [][]vec{pts}, true
	}
	walk := startDashWalk(pattern, offset, total)
	for i := 1; i < len(pts) && walk.steps <= dashStepLimit; i++ {
		walk.advance(pts[i-1], pts[i])
	}
	return walk.finished(), walk.steps <= dashStepLimit
}

func startDashWalk(pattern []float64, offset, total float64) dashWalk {
	phase := math.Mod(offset, total)
	if phase < 0 {
		phase += total
	}
	idx := 0
	for phase >= pattern[idx] {
		phase -= pattern[idx]
		idx = (idx + 1) % len(pattern)
	}
	return dashWalk{pattern: pattern, idx: idx, remaining: pattern[idx] - phase}
}

type dashWalk struct {
	pattern   []float64
	idx       int
	remaining float64
	current   []vec
	dashes    [][]vec
	steps     int
}

func (w *dashWalk) on() bool { return w.idx%2 == 0 }

func (w *dashWalk) finished() [][]vec {
	if w.on() && len(w.current) > 1 {
		w.dashes = append(w.dashes, w.current)
	}
	return w.dashes
}

func (w *dashWalk) advance(a, b vec) {
	seg := b.sub(a).length()
	if w.on() && len(w.current) == 0 {
		w.current = []vec{a}
	}
	pos := 0.0
	for seg-pos > w.remaining && w.steps <= dashStepLimit {
		w.steps++
		pos += w.remaining
		p := lerp(a, b, pos/seg)
		if w.on() {
			w.dashes = append(w.dashes, append(w.current, p))
			w.current = nil
		} else {
			w.current = []vec{p}
		}
		w.idx = (w.idx + 1) % len(w.pattern)
		w.remaining = w.pattern[w.idx]
	}
	w.remaining -= seg - pos
	if w.on() {
		w.current = append(w.current, b)
	}
}
