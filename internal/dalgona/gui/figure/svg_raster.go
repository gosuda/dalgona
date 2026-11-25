package figure

import (
	"math"
	"slices"
)

const (
	rasterPixelLimit = 4 << 20
	rasterByteBudget = 12 << 20
	rasterSubsamples = 4
)

type rasterPaint func(p vec) rgba

type edge struct {
	x0, y0, x1, y1 float64
	dir            float64
}

type raster struct {
	lo         vec
	resolution float64
	width      int
	height     int
	rule       fillRule
	paint      rasterPaint
	edges      []edge
	tiles      []Tile
	band       []*Tile
}

func newRaster(geom *shape, rule fillRule, resolution float64) (*raster, bool) {
	lo, hi, ok := geom.bounds()
	if !ok || resolution <= 0 {
		return nil, false
	}
	w := math.Ceil((hi.x - lo.x) * resolution)
	h := math.Ceil((hi.y - lo.y) * resolution)
	if w*h > rasterPixelLimit {
		return nil, false
	}
	r := &raster{lo: lo, resolution: resolution, width: max(int(w), 1), height: max(int(h), 1), rule: rule}
	for _, line := range geom.flatten(0.2 / resolution) {
		r.addPolygon(line.points)
	}
	slices.SortFunc(r.edges, func(a, b edge) int { return compareFloat(a.y0, b.y0) })
	return r, true
}

func compareFloat(a, b float64) int {
	switch {
	case a < b:
		return -1
	case a > b:
		return 1
	}
	return 0
}

func (r *raster) addPolygon(pts []vec) {
	for i := range pts {
		a, b := r.pixel(pts[i]), r.pixel(pts[(i+1)%len(pts)])
		if a.y == b.y {
			continue
		}
		e := edge{x0: a.x, y0: a.y, x1: b.x, y1: b.y, dir: 1}
		if a.y > b.y {
			e = edge{x0: b.x, y0: b.y, x1: a.x, y1: a.y, dir: -1}
		}
		r.edges = append(r.edges, e)
	}
}

func (r *raster) pixel(v vec) vec { return v.sub(r.lo).mul(r.resolution) }

func (r *raster) local(px, py float64) vec {
	return vec{r.lo.x + px/r.resolution, r.lo.y + py/r.resolution}
}

func (r *raster) render(paint rasterPaint) []Tile {
	r.paint = paint
	coverage := make([]float64, r.width+1)
	var active []edge
	next := 0
	for y := range r.height {
		clear(coverage)
		for s := range rasterSubsamples {
			sy := float64(y) + (float64(s)+0.5)/rasterSubsamples
			for next < len(r.edges) && r.edges[next].y0 <= sy {
				active = append(active, r.edges[next])
				next++
			}
			active = slices.DeleteFunc(active, func(e edge) bool { return e.y1 <= sy })
			r.scanline(active, sy, coverage)
		}
		r.writeRow(y, coverage)
		if (y+1)%TileSideLimit == 0 || y == r.height-1 {
			r.flushBand()
		}
	}
	return r.tiles
}

type crossing struct {
	x   float64
	dir float64
}

func (r *raster) scanline(active []edge, sy float64, coverage []float64) {
	crossings := make([]crossing, 0, len(active))
	for _, e := range active {
		if sy < e.y0 {
			continue
		}
		x := e.x0 + (sy-e.y0)*(e.x1-e.x0)/(e.y1-e.y0)
		crossings = append(crossings, crossing{x: x, dir: e.dir})
	}
	slices.SortFunc(crossings, func(a, b crossing) int { return compareFloat(a.x, b.x) })
	winding := 0.0
	for i := 0; i+1 < len(crossings); i++ {
		winding += crossings[i].dir
		if r.inside(winding) {
			r.span(crossings[i].x, crossings[i+1].x, coverage)
		}
	}
}

func (r *raster) inside(winding float64) bool {
	if r.rule == ruleEvenOdd {
		return int(math.Abs(winding))%2 == 1
	}
	return winding != 0
}

func (r *raster) span(a, b float64, coverage []float64) {
	a = math.Max(a, 0)
	b = math.Min(b, float64(r.width))
	for px := int(a); float64(px) < b && px < r.width; px++ {
		overlap := math.Min(b, float64(px+1)) - math.Max(a, float64(px))
		coverage[px] += overlap / rasterSubsamples
	}
}

func (r *raster) writeRow(y int, coverage []float64) {
	if len(r.band) == 0 {
		r.band = make([]*Tile, (r.width+TileSideLimit-1)/TileSideLimit)
	}
	ty := y % TileSideLimit
	for x, c := range coverage[:r.width] {
		if c <= 0 {
			continue
		}
		col := r.paint(r.local(float64(x)+0.5, float64(y)+0.5))
		alpha := clamp01(c) * col.a
		if alpha <= 0 {
			continue
		}
		tile := r.tile(x/TileSideLimit, y)
		i := (ty*tile.Width + x%TileSideLimit) * 4
		tile.Pix[i], tile.Pix[i+1], tile.Pix[i+2], tile.Pix[i+3] = channel(col.r*alpha), channel(col.g*alpha), channel(col.b*alpha), channel(alpha)
	}
}

func (r *raster) tile(tx, y int) *Tile {
	if t := r.band[tx]; t != nil {
		return t
	}
	top := y - y%TileSideLimit
	w := min(TileSideLimit, r.width-tx*TileSideLimit)
	h := min(TileSideLimit, r.height-top)
	topLeft := r.local(float64(tx*TileSideLimit), float64(top))
	bottomRight := r.local(float64(tx*TileSideLimit+w), float64(top+h))
	t := &Tile{Bounds: Rect{Min: topLeft.point(), Max: bottomRight.point()}, Width: w, Height: h, Pix: make([]byte, w*h*4)}
	r.band[tx] = t
	return t
}

func (r *raster) flushBand() {
	for i, t := range r.band {
		if t != nil {
			r.tiles = append(r.tiles, *t)
			r.band[i] = nil
		}
	}
}

type gradientDef struct {
	radial bool
	x1, y1 float64
	x2, y2 float64
	cx, cy float64
	r      float64
	fx, fy float64
	fr     float64
	spread Spread
	stops  []stopDef
	toUser matrix
}

type stopDef struct {
	offset float64
	color  rgba
}

func (g *gradientDef) colorAt(t float64) rgba {
	t = spreadOffset(t, g.spread)
	stops := g.stops
	if t <= stops[0].offset {
		return stops[0].color
	}
	for i := 1; i < len(stops); i++ {
		if t <= stops[i].offset {
			a, b := stops[i-1], stops[i]
			span := b.offset - a.offset
			if span <= 0 {
				return b.color
			}
			return mixPremultiplied(a.color, b.color, (b.offset-t)/span, (t-a.offset)/span)
		}
	}
	return stops[len(stops)-1].color
}

func spreadOffset(t float64, spread Spread) float64 {
	switch spread {
	case SpreadRepeat:
		return t - math.Floor(t)
	case SpreadReflect:
		m := math.Mod(math.Abs(t), 2)
		if m > 1 {
			return 2 - m
		}
		return m
	case SpreadPad:
	}
	return clamp01(t)
}

func (g *gradientDef) rasterPaint() (rasterPaint, bool) {
	inverse, ok := g.toUser.invert()
	if !ok {
		return nil, false
	}
	if !g.radial {
		dx, dy := g.x2-g.x1, g.y2-g.y1
		den := dx*dx + dy*dy
		return func(p vec) rgba {
			q := inverse.vec(p)
			if den == 0 {
				return g.colorAt(1)
			}
			return g.colorAt(((q.x-g.x1)*dx + (q.y-g.y1)*dy) / den)
		}, true
	}
	return func(p vec) rgba {
		t, ok := g.radialOffset(inverse.vec(p))
		if !ok {
			return rgba{}
		}
		return g.colorAt(t)
	}, true
}

func (g *gradientDef) radialOffset(p vec) (float64, bool) {
	cd := vec{g.cx - g.fx, g.cy - g.fy}
	pd := vec{p.x - g.fx, p.y - g.fy}
	dr := g.r - g.fr
	a := cd.dot(cd) - dr*dr
	b := pd.dot(cd) + g.fr*dr
	c := pd.dot(pd) - g.fr*g.fr
	if math.Abs(a) < 1e-12 {
		if b == 0 {
			return 0, false
		}
		return c / (2 * b), true
	}
	disc := b*b - a*c
	if disc < 0 {
		return 0, false
	}
	sq := math.Sqrt(disc)
	for _, t := range [...]float64{(b + sq) / a, (b - sq) / a} {
		if g.fr+t*dr >= 0 {
			return t, true
		}
	}
	return 0, false
}
