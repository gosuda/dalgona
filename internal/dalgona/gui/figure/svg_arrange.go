package figure

import (
	"math"
	"slices"
)

const (
	arrangeEdgeLimit  = 16_384
	arrangeEventLimit = 65_536
)

type sweepEdge struct {
	from, to vec
	winding  int
	layer    int
}

func (e sweepEdge) xAt(y float64) float64 {
	return e.from.x + (y-e.from.y)*(e.to.x-e.from.x)/(e.to.y-e.from.y)
}

type arrangement struct {
	edges  []sweepEdge
	layers int
}

type classArea struct {
	key  string
	area *shape
}

type cellClass func(winding []int) (string, bool)

func (a *arrangement) addShape(s *shape, tolerance float64) int {
	layer := a.layers
	a.layers++
	for _, line := range s.flatten(tolerance) {
		for i, p := range line.points {
			a.addEdge(p, line.points[(i+1)%len(line.points)], layer)
		}
	}
	return layer
}

func (a *arrangement) addEdge(p, q vec, layer int) {
	switch {
	case p.y < q.y:
		a.edges = append(a.edges, sweepEdge{from: p, to: q, winding: 1, layer: layer})
	case p.y > q.y:
		a.edges = append(a.edges, sweepEdge{from: q, to: p, winding: -1, layer: layer})
	}
}

func crossingHeight(e, f sweepEdge) (float64, bool) {
	lo, hi := math.Max(e.from.y, f.from.y), math.Min(e.to.y, f.to.y)
	if hi <= lo {
		return 0, false
	}
	d0, d1 := e.xAt(lo)-f.xAt(lo), e.xAt(hi)-f.xAt(hi)
	if d0*d1 >= 0 {
		return 0, false
	}
	return lo + (hi-lo)*d0/(d0-d1), true
}

func (a *arrangement) events() ([]float64, bool) {
	slices.SortFunc(a.edges, func(e, f sweepEdge) int { return compareFloat(e.from.y, f.from.y) })
	var events []float64
	for i, e := range a.edges {
		events = append(events, e.from.y, e.to.y)
		for _, f := range a.edges[i+1:] {
			if f.from.y >= e.to.y {
				break
			}
			if y, ok := crossingHeight(e, f); ok {
				events = append(events, y)
			}
		}
		if len(events) > arrangeEventLimit {
			return nil, false
		}
	}
	slices.Sort(events)
	return slices.Compact(events), true
}

type sweep struct {
	classify cellClass
	winding  []int
	areas    map[string]*shape
	verbs    int
}

func (a *arrangement) regions(classify cellClass) ([]classArea, bool) {
	if len(a.edges) > arrangeEdgeLimit {
		return nil, false
	}
	events, ok := a.events()
	if !ok {
		return nil, false
	}
	s := &sweep{classify: classify, winding: make([]int, a.layers), areas: map[string]*shape{}}
	var active []sweepEdge
	next := 0
	for i := 0; i+1 < len(events); i++ {
		top, bottom := events[i], events[i+1]
		for next < len(a.edges) && a.edges[next].from.y <= top {
			active = append(active, a.edges[next])
			next++
		}
		active = slices.DeleteFunc(active, func(e sweepEdge) bool { return e.to.y <= top })
		if bottom-top > 1e-9 {
			s.slab(active, top, bottom)
		}
		if s.verbs > OperationLimit {
			return nil, false
		}
	}
	return s.collect(), true
}

type slabRun struct {
	key         string
	keep        bool
	left, right sweepEdge
}

func (s *sweep) slab(active []sweepEdge, top, bottom float64) {
	mid := (top + bottom) / 2
	slices.SortFunc(active, func(e, f sweepEdge) int { return compareFloat(e.xAt(mid), f.xAt(mid)) })
	clear(s.winding)
	run := slabRun{}
	for j := 0; j+1 < len(active); j++ {
		s.winding[active[j].layer] += active[j].winding
		key, keep := s.classify(s.winding)
		if keep && run.keep && key == run.key {
			run.right = active[j+1]
			continue
		}
		s.flush(run, top, bottom)
		run = slabRun{key: key, keep: keep, left: active[j], right: active[j+1]}
	}
	s.flush(run, top, bottom)
}

func (s *sweep) flush(run slabRun, top, bottom float64) {
	if !run.keep {
		return
	}
	topLeft, topRight := run.left.xAt(top), run.right.xAt(top)
	bottomLeft, bottomRight := run.left.xAt(bottom), run.right.xAt(bottom)
	if topRight-topLeft <= 1e-12 && bottomRight-bottomLeft <= 1e-12 {
		return
	}
	area := s.areas[run.key]
	if area == nil {
		area = &shape{}
		s.areas[run.key] = area
	}
	area.moveTo(vec{topLeft, top})
	area.lineTo(vec{topRight, top})
	area.lineTo(vec{bottomRight, bottom})
	area.lineTo(vec{bottomLeft, bottom})
	area.close()
	s.verbs += 5
}

func (s *sweep) collect() []classArea {
	out := make([]classArea, 0, len(s.areas))
	for key, area := range s.areas {
		out = append(out, classArea{key: key, area: area})
	}
	slices.SortFunc(out, func(a, b classArea) int {
		switch {
		case a.key < b.key:
			return -1
		case a.key > b.key:
			return 1
		}
		return 0
	})
	return out
}

func (r fillRule) covers(winding int) bool {
	if r == ruleEvenOdd {
		return winding%2 != 0
	}
	return winding != 0
}

func solidArea(geom *shape, rule fillRule, tolerance float64) (*shape, bool) {
	a := &arrangement{}
	a.addShape(geom, tolerance)
	areas, ok := a.regions(func(w []int) (string, bool) { return "", rule.covers(w[0]) })
	if !ok {
		return nil, false
	}
	if len(areas) == 0 {
		return &shape{}, true
	}
	return areas[0].area, true
}
