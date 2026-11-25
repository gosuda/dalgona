package figure

import (
	"math"
	"strings"
	"testing"
)

func decodeRaw(t *testing.T, src string) *List {
	t.Helper()
	l, err := DecodeSVG([]byte(src), DecodeOptions{Profile: ProfileSVG, Scale: 1})
	if err != nil {
		t.Fatalf("decode: %v\n%s", err, src)
	}
	if err := l.Validate(); err != nil {
		t.Fatalf("decoded list is invalid: %v", err)
	}
	return l
}

func svgDoc(body string) string {
	return `<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="200" height="100">` + body + `</svg>`
}

func opsOf(l *List, kind OpKind) []Op {
	var out []Op
	for _, o := range l.Ops {
		if o.Kind == kind {
			out = append(out, o)
		}
	}
	return out
}

func pathBounds(p Path) Rect {
	r := Rect{Min: Point{math.MaxFloat32, math.MaxFloat32}, Max: Point{-math.MaxFloat32, -math.MaxFloat32}}
	for _, pt := range p.Points {
		r.Min.X, r.Min.Y = min(r.Min.X, pt.X), min(r.Min.Y, pt.Y)
		r.Max.X, r.Max.Y = max(r.Max.X, pt.X), max(r.Max.Y, pt.Y)
	}
	return r
}

func near(a, b, tolerance float32) bool { return float32(math.Abs(float64(a-b))) <= tolerance }

func nearRect(r, want Rect, tolerance float32) bool {
	return near(r.Min.X, want.Min.X, tolerance) && near(r.Min.Y, want.Min.Y, tolerance) && near(r.Max.X, want.Max.X, tolerance) && near(r.Max.Y, want.Max.Y, tolerance)
}

func lastFill(t *testing.T, l *List) (Op, Path) {
	t.Helper()
	fills := opsOf(l, OpFill)
	if len(fills) == 0 {
		t.Fatal("decoded figure has no fill")
	}
	o := fills[len(fills)-1]
	return o, l.Paths[o.Index]
}

func soleOp(t *testing.T, l *List, kind OpKind) Op {
	t.Helper()
	ops := opsOf(l, kind)
	if len(ops) != 1 {
		t.Fatalf("display list has %d operations of kind %d, want one: %+v", len(ops), kind, ops)
	}
	return ops[0]
}

func TestDecodeGeometryElements(t *testing.T) {
	cases := []struct {
		name           string
		body           string
		x0, y0, x1, y1 float32
	}{
		{"rect", `<rect x="10" y="20" width="30" height="40"/>`, 10, 20, 40, 60},
		{"rounded rect", `<rect x="0" y="0" width="30" height="20" rx="5"/>`, 0, 0, 30, 20},
		{"circle", `<circle cx="50" cy="50" r="10"/>`, 40, 40, 60, 60},
		{"ellipse", `<ellipse cx="50" cy="50" rx="20" ry="5"/>`, 30, 45, 70, 55},
		{"polygon", `<polygon points="0,0 10,0 10,20"/>`, 0, 0, 10, 20},
		{"polyline", `<polyline points="5 5, 15 25 30 5"/>`, 5, 5, 30, 25},
		{"absolute path", `<path d="M10 10 H30 V30 H10 Z"/>`, 10, 10, 30, 30},
		{"relative path", `<path d="m10 10 h20 v20 h-20 z"/>`, 10, 10, 30, 30},
		{"implicit lineto", `<path d="M0 0 10 0 10 10"/>`, 0, 0, 10, 10},
		{"compact numbers", `<path d="M.5.5L10-2e1"/>`, 0.5, -20, 10, 0.5},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			_, p := lastFill(t, decodeRaw(t, svgDoc(c.body)))
			if got := pathBounds(p); !nearRect(got, Rect{Min: Point{c.x0, c.y0}, Max: Point{c.x1, c.y1}}, 1e-3) {
				t.Fatalf("bounds = %+v, want (%g,%g)-(%g,%g)", got, c.x0, c.y0, c.x1, c.y1)
			}
		})
	}
}

func curveExtent(t *testing.T, d string) Rect {
	t.Helper()
	s, problem := parsePathData(d)
	if problem != "" {
		t.Fatal(problem)
	}
	lo, hi, _ := s.bounds()
	return Rect{Min: lo.point(), Max: hi.point()}
}

func TestDecodePathCurvesAndArcs(t *testing.T) {
	cases := []struct {
		name           string
		d              string
		x0, y0, x1, y1 float32
	}{
		{"cubic extremum", "M0 0 C0 40 40 40 40 0", 0, 0, 40, 30},
		{"smooth cubic reflects", "M0 0 C0 40 40 40 40 0 S80 -40 80 0", 0, -30, 80, 30},
		{"quadratic extremum", "M0 0 Q20 40 40 0", 0, 0, 40, 20},
		{"smooth quadratic reflects", "M0 0 Q20 40 40 0 T80 0", 0, -20, 80, 20},
		{"clockwise half circle", "M0 0 A10 10 0 0 1 20 0", 0, -10, 20, 0},
		{"counterclockwise half circle", "M0 0 A10 10 0 0 0 20 0", 0, 0, 20, 10},
		{"radii scale up to reach", "M0 0 A1 1 0 0 1 20 0", 0, -10, 20, 0},
		{"large arc", "M0 0 A10 10 0 1 1 10 10", 0, -10, 20, 10},
		{"compact arc flags", "M0 0a10 10 0 0120 0", 0, -10, 20, 0},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := curveExtent(t, c.d); !nearRect(got, Rect{Min: Point{c.x0, c.y0}, Max: Point{c.x1, c.y1}}, 0.05) {
				t.Fatalf("extent of %q = %+v, want (%g,%g)-(%g,%g)", c.d, got, c.x0, c.y0, c.x1, c.y1)
			}
		})
	}
}

func TestDecodeViewBoxAndUnits(t *testing.T) {
	l := decodeRaw(t, `<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 10 10"><rect width="10" height="10"/></svg>`)
	transforms := opsOf(l, OpTransform)
	if len(transforms) != 1 || transforms[0].Transform != (Affine{A: 10, D: 10, E: 50}) {
		t.Fatalf("viewBox transform = %+v, want scale 10 centered at x 50", transforms)
	}
	l = decodeRaw(t, `<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 10 10" preserveAspectRatio="xMaxYMin slice"><rect width="1" height="1"/></svg>`)
	if got := opsOf(l, OpTransform)[0].Transform; got != (Affine{A: 20, D: 20, E: 0, F: 0}) {
		t.Fatalf("slice transform = %+v, want scale 20", got)
	}
	l = decodeRaw(t, `<svg xmlns="http://www.w3.org/2000/svg" width="1in" height="12pt"/>`)
	if l.Width != 96 || l.Height != 16 {
		t.Fatalf("size = %gx%g, want 96x16", l.Width, l.Height)
	}
	units := []struct {
		width string
		want  float32
	}{{"1pc", 16}, {"2.54cm", 96}, {"25.4mm", 96}, {"2em", 40}, {"2ex", 20}, {"50%", 100}}
	for _, u := range units {
		l := decodeRaw(t, svgDoc(`<g font-size="20"><rect width="`+u.width+`" height="1"/></g>`))
		_, p := lastFill(t, l)
		if got := pathBounds(p).Max.X; !near(got, u.want, 1e-3) {
			t.Fatalf("width %s = %g, want %g", u.width, got, u.want)
		}
	}
}

func TestDecodeNestedTransforms(t *testing.T) {
	l := decodeRaw(t, svgDoc(`<g transform="translate(5 7)"><g transform="rotate(90) scale(2,3) skewX(0)"><rect width="1" height="1"/></g></g>`))
	transforms := opsOf(l, OpTransform)
	if len(transforms) != 2 {
		t.Fatalf("transforms = %+v, want two nested groups", transforms)
	}
	inner := transforms[1].Transform
	if !near(inner.A, 0, 1e-6) || !near(inner.B, 2, 1e-6) || !near(inner.C, -3, 1e-6) || !near(inner.D, 0, 1e-6) {
		t.Fatalf("rotate(90) scale(2,3) = %+v, want [0 2 -3 0]", inner)
	}
	if transforms[0].Transform != (Affine{A: 1, D: 1, E: 5, F: 7}) {
		t.Fatalf("outer transform = %+v", transforms[0].Transform)
	}
}

func TestDecodePaint(t *testing.T) {
	l := decodeRaw(t, svgDoc(`<g color="#00ff00"><rect width="1" height="1" fill="currentColor" fill-opacity="0.5"/></g>`))
	o, _ := lastFill(t, l)
	if o.Paint.Color != (Color{0, 255, 0, 128}) {
		t.Fatalf("currentColor fill = %+v, want half-opaque green", o.Paint.Color)
	}
	l = decodeRaw(t, svgDoc(`<rect width="1" height="1" style="fill: rgb(255 0 0 / 25%)"/>`))
	if o, _ := lastFill(t, l); o.Paint.Color != (Color{255, 0, 0, 64}) {
		t.Fatalf("style fill = %+v, want quarter-opaque red", o.Paint.Color)
	}
	l = decodeRaw(t, svgDoc(`<g opacity="0.5"><rect width="1" height="1"/></g>`))
	if ops := opsOf(l, OpOpacity); len(ops) != 1 || ops[0].Alpha != 0.5 {
		t.Fatalf("group opacity ops = %+v", ops)
	}
}

func strokeBounds(t *testing.T, body string) Rect {
	t.Helper()
	_, p := lastFill(t, decodeRaw(t, svgDoc(body)))
	return pathBounds(p)
}

func TestDecodeStrokeCapsAndDashes(t *testing.T) {
	if got := strokeBounds(t, `<line x1="10" y1="10" x2="30" y2="10" stroke="black" stroke-width="4"/>`); !nearRect(got, Rect{Min: Point{10, 8}, Max: Point{30, 12}}, 1e-3) {
		t.Fatalf("butt line outline = %+v, want (10,8)-(30,12)", got)
	}
	if got := strokeBounds(t, `<line x1="10" y1="10" x2="30" y2="10" stroke="black" stroke-width="4" stroke-linecap="square"/>`); !nearRect(got, Rect{Min: Point{8, 8}, Max: Point{32, 12}}, 1e-3) {
		t.Fatalf("square cap outline = %+v, want (8,8)-(32,12)", got)
	}
	_, solid := lastFill(t, decodeRaw(t, svgDoc(`<line x2="100" stroke="black"/>`)))
	_, dashed := lastFill(t, decodeRaw(t, svgDoc(`<line x2="100" stroke="black" stroke-dasharray="10 10"/>`)))
	if moves(solid) != 1 || moves(dashed) != 5 {
		t.Fatalf("dash subpaths = %d (solid %d), want 5 dashes over 100 units", moves(dashed), moves(solid))
	}
	_, odd := lastFill(t, decodeRaw(t, svgDoc(`<line x2="100" stroke="black" stroke-dasharray="10 5 5"/>`)))
	if moves(odd) != 8 {
		t.Fatalf("odd dash array produced %d dashes, want the repeated pattern's 8", moves(odd))
	}
}

func TestDecodeStrokeJoinsHonourTheMiterLimit(t *testing.T) {
	miter := strokeBounds(t, `<polyline points="0,0 20,0 20,20" fill="none" stroke="black" stroke-width="4"/>`)
	if !near(miter.Min.Y, -2, 1e-3) || !near(miter.Max.X, 22, 1e-3) {
		t.Fatalf("miter join outline = %+v, want the corner tip at (22,-2)", miter)
	}
	bevel := strokeBounds(t, `<polyline points="0,0 20,0 20,20" fill="none" stroke="black" stroke-width="4" stroke-miterlimit="1"/>`)
	l := decodeRaw(t, svgDoc(`<polyline points="0,0 20,0 20,20" fill="none" stroke="black" stroke-width="4" stroke-miterlimit="1"/>`))
	_, p := lastFill(t, l)
	for _, pt := range p.Points {
		if near(pt.X, 22, 1e-3) && near(pt.Y, -2, 1e-3) {
			t.Fatalf("miter limit 1 still produced the miter tip; outline %+v", bevel)
		}
	}
}

func moves(p Path) int {
	n := 0
	for _, v := range p.Verbs {
		if v == VerbMove {
			n++
		}
	}
	return n
}

func tilePixel(l *List, x, y float32) (Color, bool) {
	for _, t := range l.Tiles {
		if x < t.Bounds.Min.X || x >= t.Bounds.Max.X || y < t.Bounds.Min.Y || y >= t.Bounds.Max.Y {
			continue
		}
		px := int((x - t.Bounds.Min.X) / t.Bounds.Width() * float32(t.Width))
		py := int((y - t.Bounds.Min.Y) / t.Bounds.Height() * float32(t.Height))
		i := (py*t.Width + px) * 4
		return Color{t.Pix[i], t.Pix[i+1], t.Pix[i+2], t.Pix[i+3]}, true
	}
	return Color{}, false
}

func TestDecodeEvenOddFillRasterizes(t *testing.T) {
	l := decodeRaw(t, svgDoc(`<path fill="#0000ff" fill-rule="evenodd" d="M0 0 H40 V40 H0 Z M10 10 H30 V30 H10 Z"/>`))
	if len(opsOf(l, OpFill)) != 0 || len(opsOf(l, OpImage)) == 0 {
		t.Fatalf("evenodd fill must become raster tiles; ops %+v", l.Ops)
	}
	if c, _ := tilePixel(l, 20, 20); c.A != 0 {
		t.Fatalf("evenodd hole pixel = %+v, want transparent", c)
	}
	if c, _ := tilePixel(l, 5, 5); c != (Color{0, 0, 255, 255}) {
		t.Fatalf("evenodd ring pixel = %+v, want opaque blue", c)
	}
	nonzero := decodeRaw(t, svgDoc(`<path fill="#0000ff" d="M0 0 H40 V40 H0 Z M10 10 H30 V30 H10 Z"/>`))
	if len(opsOf(nonzero, OpFill)) != 1 {
		t.Fatal("nonzero fill must stay a vector fill")
	}
}

func TestDecodeDefinitions(t *testing.T) {
	l := decodeRaw(t, svgDoc(`<defs><rect id="r" width="4" height="4"/></defs><use href="#r" x="10" y="20"/><use xlink:href="#r" x="1"/>`))
	if fills := opsOf(l, OpFill); len(fills) != 2 {
		t.Fatalf("use instances = %d, want 2", len(fills))
	}
	if ts := opsOf(l, OpTransform); len(ts) != 2 || ts[0].Transform != (Affine{A: 1, D: 1, E: 10, F: 20}) {
		t.Fatalf("use placements = %+v", ts)
	}
	l = decodeRaw(t, svgDoc(`<symbol id="s" viewBox="0 0 2 2"><rect width="2" height="2"/></symbol><use href="#s" width="20" height="20"/>`))
	if ts := opsOf(l, OpTransform); len(ts) != 1 || ts[0].Transform.A != 10 {
		t.Fatalf("symbol viewBox scale = %+v, want 10", ts)
	}
	l = decodeRaw(t, svgDoc(`<clipPath id="c"><circle cx="5" cy="5" r="5"/></clipPath><rect width="100" height="100" clip-path="url(#c)"/>`))
	clips := opsOf(l, OpClip)
	if len(clips) != 2 || !nearRect(pathBounds(l.Paths[clips[1].Index]), Rect{Min: Point{0, 0}, Max: Point{10, 10}}, 1e-3) {
		t.Fatalf("clip ops = %+v, want the root viewport clip and the circle clip", clips)
	}
	l = decodeRaw(t, svgDoc(`<clipPath id="b" clipPathUnits="objectBoundingBox"><rect width="0.5" height="1"/></clipPath><rect x="10" y="10" width="20" height="20" clip-path="url(#b)"/>`))
	if got := pathBounds(l.Paths[opsOf(l, OpClip)[1].Index]); !nearRect(got, Rect{Min: Point{10, 10}, Max: Point{20, 30}}, 1e-3) {
		t.Fatalf("objectBoundingBox clip = %+v, want the left half", got)
	}
}

func windingAt(p Path, x, y float32) int {
	winding := 0
	var start, prev Point
	k := 0
	edge := func(a, b Point) {
		if (a.Y <= y) != (b.Y <= y) {
			cross := a.X + (y-a.Y)*(b.X-a.X)/(b.Y-a.Y)
			if cross > x {
				if b.Y > a.Y {
					winding++
				} else {
					winding--
				}
			}
		}
	}
	for _, v := range p.Verbs {
		switch v {
		case VerbMove:
			start, prev = p.Points[k], p.Points[k]
		case VerbLine:
			edge(prev, p.Points[k])
			prev = p.Points[k]
		case VerbQuad, VerbCube:
			last := p.Points[k+verbPoints[v]-1]
			edge(prev, last)
			prev = last
		case VerbClose:
			edge(prev, start)
			prev = start
		}
		k += verbPoints[v]
	}
	return winding
}

func TestDecodeMarkersFollowPathDirection(t *testing.T) {
	l := decodeRaw(t, svgDoc(`<marker id="m" markerWidth="4" markerHeight="4" refX="2" refY="2" orient="auto" markerUnits="userSpaceOnUse"><rect width="4" height="4"/></marker><line x1="0" y1="0" x2="0" y2="50" stroke="black" marker-end="url(#m)"/>`))
	ts := opsOf(l, OpTransform)
	if len(ts) != 1 {
		t.Fatalf("marker transforms = %+v", ts)
	}
	m := ts[0].Transform
	if !near(m.A, 0, 1e-6) || !near(m.B, 1, 1e-6) || !near(m.E, 2, 1e-4) || !near(m.F, 48, 1e-4) {
		t.Fatalf("downward marker transform = %+v, want a 90 degree turn placing ref (2,2) at (0,50)", m)
	}
	reverse := decodeRaw(t, svgDoc(`<marker id="m" orient="auto-start-reverse" markerUnits="userSpaceOnUse"><rect width="1" height="1"/></marker><line x2="10" stroke="black" marker-start="url(#m)"/>`))
	if m := opsOf(reverse, OpTransform)[0].Transform; !near(m.A, -1, 1e-6) {
		t.Fatalf("auto-start-reverse transform = %+v, want a 180 degree turn", m)
	}
}

func TestDecodeLinearGradient(t *testing.T) {
	l := decodeRaw(t, svgDoc(`<linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="100%" stop-color="blue" stop-opacity="0.5"/></linearGradient><rect x="10" width="100" height="10" fill="url(#g)"/>`))
	o, _ := lastFill(t, l)
	if o.Paint.Gradient != 0 || len(l.Gradients) != 1 {
		t.Fatalf("gradient paint = %+v", o.Paint)
	}
	g := l.Gradients[0]
	if g.Space != (Affine{A: 100, D: 10, E: 10}) || g.End != (Point{X: 1}) {
		t.Fatalf("objectBoundingBox gradient space = %+v end %+v", g.Space, g.End)
	}
	first, last := g.Stops[0], g.Stops[len(g.Stops)-1]
	if first.Color != (Color{255, 0, 0, 255}) || last.Color != (Color{0, 0, 255, 128}) || last.Offset != 1 {
		t.Fatalf("stops = %+v", g.Stops)
	}
}

func TestDecodeRadialGradient(t *testing.T) {
	l := decodeRaw(t, svgDoc(`<radialGradient id="r" gradientUnits="userSpaceOnUse" cx="20" cy="20" r="20"><stop offset="0" stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></radialGradient><rect width="40" height="40" fill="url(#r)"/>`))
	if c, ok := tilePixel(l, 20, 20); !ok || c.R < 240 || c.B > 15 {
		t.Fatalf("radial center pixel = %+v, want the first stop", c)
	}
	if c, _ := tilePixel(l, 39.5, 20); c.B < 230 {
		t.Fatalf("radial edge pixel = %+v, want the last stop", c)
	}
}

const spannedText = `<text x="50" y="20" text-anchor="middle" font-weight="bold" font-family="Inter, sans-serif" dominant-baseline="central" font-size="12">  First
  line<tspan x="50" dy="1.5em" font-style="italic">second 한글</tspan></text>`

func TestDecodeTextChunksAndLabels(t *testing.T) {
	l := decodeRaw(t, svgDoc(spannedText))
	if len(l.Chunks) != 2 || len(l.Labels) != 1 {
		t.Fatalf("chunks %d labels %q, want two chunks and one label", len(l.Chunks), l.Labels)
	}
	if l.Labels[0] != "First line\nsecond 한글" {
		t.Fatalf("label = %q", l.Labels[0])
	}
	first := l.Chunks[0]
	if first.Anchor != AnchorMiddle || first.Origin != (Point{50, 20}) || first.Runs[0].Weight != 700 || first.Runs[0].Baseline != BaselineCentral {
		t.Fatalf("first chunk = %+v", first)
	}
	mono := decodeRaw(t, svgDoc(`<text font-family="'JetBrains Mono', monospace">x</text>`))
	if mono.Chunks[0].Runs[0].Face != FaceMono {
		t.Fatal("monospace family did not select the mono face")
	}
}

func TestDecodeTspanStartsAShiftedItalicChunk(t *testing.T) {
	l := decodeRaw(t, svgDoc(spannedText))
	if len(l.Chunks) != 2 {
		t.Fatalf("chunks %d, want the text chunk and the tspan chunk", len(l.Chunks))
	}
	second := l.Chunks[1]
	if second.Origin != (Point{50, 20}) || second.Runs[0].Shift != (Point{0, 18}) || !second.Runs[0].Italic || second.Runs[0].Text != "second 한글" {
		t.Fatalf("second chunk = %+v", second)
	}
}

func TestDecodeEngineStylesResolve(t *testing.T) {
	src := `<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10" style="--bg:#FFFFFF;--fg:#000000;background:var(--bg)">
<style>@import url('https://fonts.example/css?family=A;B');
svg { --_line: var(--line, color-mix(in srgb, var(--fg) 50%, var(--bg))); }
.edge.thick { stroke-width: 3; }
.d2 .fill-x { fill: #102030; }</style>
<g class="d2"><rect class="fill-x" width="1" height="1" fill="#ffffff"/></g>
<line class="edge thick" x2="5" stroke="var(--_line)"/></svg>`
	l, err := DecodeSVG([]byte(src), DecodeOptions{Profile: ProfileMermaid})
	if err != nil {
		t.Fatal(err)
	}
	fills := opsOf(l, OpFill)
	if len(fills) != 3 || fills[0].Paint.Color != (Color{255, 255, 255, 255}) {
		t.Fatalf("background fill = %+v, want white first", fills)
	}
	if fills[1].Paint.Color != (Color{0x10, 0x20, 0x30, 255}) {
		t.Fatalf("class rule fill = %+v, want the stylesheet colour over the attribute", fills[1].Paint.Color)
	}
	if fills[2].Paint.Color != (Color{128, 128, 128, 255}) {
		t.Fatalf("color-mix stroke = %+v, want 50%% grey", fills[2].Paint.Color)
	}
	if b := pathBounds(l.Paths[fills[2].Index]); !near(b.Max.Y-b.Min.Y, 3, 1e-3) {
		t.Fatalf("compound class stroke width = %g, want 3", b.Max.Y-b.Min.Y)
	}
}

func TestDecodeEngineShadowFilter(t *testing.T) {
	src := `<svg xmlns="http://www.w3.org/2000/svg" width="50" height="50"><defs><filter id="shadow-filter">
<feGaussianBlur stdDeviation="1.7 " in="SourceGraphic"></feGaussianBlur>
<feFlood flood-color="#3d4574" flood-opacity="0.4" result="F" in="SourceGraphic"></feFlood>
<feComposite in="F" in2="SourceAlpha" operator="in" result="C"></feComposite>
<feOffset dx="3" dy="5" result="O" in="C"></feOffset>
<feBlend in="SourceGraphic" in2="O" mode="normal" result="B"></feBlend></filter></defs>
<g filter="url(#shadow-filter)"><rect width="10" height="10" fill="#ffffff"/></g></svg>`
	l, err := DecodeSVG([]byte(src), DecodeOptions{Profile: ProfileD2})
	if err != nil {
		t.Fatal(err)
	}
	fills := opsOf(l, OpFill)
	if len(fills) != 2 || fills[0].Paint.Color != (Color{0x3d, 0x45, 0x74, 255}) || fills[1].Paint.Color != (Color{255, 255, 255, 255}) {
		t.Fatalf("shadow fills = %+v, want the flood silhouette under the white source", fills)
	}
	shift := soleOp(t, l, OpTransform)
	opacity := soleOp(t, l, OpOpacity)
	if shift.Transform.E != 3 || shift.Transform.F != 5 || !near(opacity.Alpha, 0.4, 1e-6) {
		t.Fatalf("shadow placement = %+v opacity %+v", shift, opacity)
	}
}

func TestDecodeRefusesOutOfProfile(t *testing.T) {
	cases := []struct {
		name string
		src  string
		code FailureCode
		word string
	}{
		{"script", svgDoc(`<script>alert(1)</script>`), FailureForbidden, "scripts"},
		{"foreignObject", svgDoc(`<foreignObject/>`), FailureForbidden, "foreign"},
		{"image", svgDoc(`<image href="x.png"/>`), FailureForbidden, "resource"},
		{"animation", svgDoc(`<rect><animate attributeName="x"/></rect>`), FailureForbidden, "animation"},
		{"event attribute", svgDoc(`<rect onclick="x()"/>`), FailureForbidden, "event"},
		{"external use", svgDoc(`<use href="other.svg#a"/>`), FailureForbidden, "outside"},
		{"external paint", svgDoc(`<rect fill="url(https://x/y#g)"/>`), FailureForbidden, "outside"},
		{"doctype", `<!DOCTYPE svg><svg xmlns="http://www.w3.org/2000/svg"/>`, FailureForbidden, "declaration"},
		{"entity", "<?xml version=\"1.0\"?>\n<!ENTITY x \"y\">\n<svg/>", FailureForbidden, "declaration"},
		{"style element", svgDoc(`<style>rect{fill:red}</style>`), FailureInvalid, "outside"},
		{"style variable", svgDoc(`<rect style="fill: var(--x)"/>`), FailureInvalid, "variables"},
		{"custom property", svgDoc(`<rect style="--x: red"/>`), FailureInvalid, "custom property"},
		{"unknown property", svgDoc(`<rect style="filter: blur(2px)"/>`), FailureInvalid, "filter"},
		{"blend mode", svgDoc(`<rect style="mix-blend-mode: multiply"/>`), FailureInvalid, "mix-blend-mode"},
		{"filter element", svgDoc(`<filter id="f"/>`), FailureInvalid, "outside"},
		{"text path", svgDoc(`<textPath/>`), FailureInvalid, "outside"},
		{"foreign namespace", `<svg xmlns="http://www.w3.org/2000/svg"><x:a xmlns:x="urn:x"/></svg>`, FailureInvalid, "namespace"},
		{"per-glyph positions", svgDoc(`<text x="1 2 3">abc</text>`), FailureInvalid, "positions"},
		{"use cycle", svgDoc(`<g id="a"><use href="#b"/></g><g id="b"><use href="#a"/></g>`), FailureInvalid, "cycle"},
		{"missing reference", svgDoc(`<use href="#nothing"/>`), FailureInvalid, "does not define"},
		{"text decoration", svgDoc(`<text text-decoration="underline">x</text>`), FailureInvalid, "text-decoration"},
		{"malformed path", svgDoc(`<path d="M0 0 L x"/>`), FailureInvalid, "malformed"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			_, err := DecodeSVG([]byte(c.src), DecodeOptions{})
			f := FailureOf(err)
			if err == nil || f.Code != c.code || !strings.Contains(f.Message, c.word) || !f.Span.Known() {
				t.Fatalf("error = %v (code %v span %+v), want %v naming %q with a location", err, f.Code, f.Span, c.code, c.word)
			}
		})
	}
}

func TestDecodeEnforcesBounds(t *testing.T) {
	deep := strings.Repeat("<g>", SVGDepthLimit+1) + strings.Repeat("</g>", SVGDepthLimit+1)
	many := strings.Repeat("<g/>", SVGElementLimit+1)
	bomb := `<defs><rect id="l0" width="1" height="1"/>`
	for i := 1; i <= 20; i++ {
		bomb += `<g id="l` + itoa(i) + `"><use href="#l` + itoa(i-1) + `"/><use href="#l` + itoa(i-1) + `"/></g>`
	}
	bomb += `</defs><use href="#l20"/>`
	cases := []struct {
		name string
		src  string
		word string
	}{
		{"nesting", svgDoc(deep), "deeper"},
		{"element count", svgDoc(many), "elements"},
		{"use expansion", svgDoc(bomb), "expands"},
		{"coordinate", svgDoc(`<rect width="2000000" height="1"/>`), "coordinate"},
		{"transformed coordinate", svgDoc(`<g transform="scale(1000)"><rect width="2000" height="1"/></g>`), "logical pixels"},
		{"non-finite transform", svgDoc(`<g transform="scale(1e400)"><rect/></g>`), "not finite"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			_, err := DecodeSVG([]byte(c.src), DecodeOptions{Profile: ProfileNomnoml})
			if err == nil || !strings.Contains(err.Error(), c.word) {
				t.Fatalf("error = %v, want a refusal naming %q", err, c.word)
			}
		})
	}
	if _, err := DecodeSVG(make([]byte, SourceLimit+1), DecodeOptions{}); codeOf(err) != FailureLimit {
		t.Fatalf("oversized source = %v, want a limit failure", err)
	}
}

func FuzzDecodeSVG(f *testing.F) {
	seeds := []string{
		svgDoc(`<rect x="1" y="2" width="3" height="4" rx="1" stroke="red" stroke-dasharray="1 2"/>`),
		svgDoc(`<path d="M0 0 C1 2 3 4 5 6 S7 8 9 10 Q1 2 3 4 T5 6 A1 2 30 1 0 7 8 Z" fill-rule="evenodd"/>`),
		svgDoc(`<defs><linearGradient id="g"><stop offset="0"/><stop offset="1" stop-color="red"/></linearGradient></defs><circle r="5" fill="url(#g)"/>`),
		svgDoc(`<marker id="m" orient="auto"><path d="M0 0 L1 1"/></marker><polyline points="0 0 1 1 2 0" marker-mid="url(#m)" stroke="black"/>`),
		svgDoc(`<clipPath id="c"><rect width="1" height="1"/></clipPath><mask id="k"><rect width="9" height="9" fill="white"/></mask><g clip-path="url(#c)" mask="url(#k)"><text x="1" y="2">t<tspan x="1" dy="1em">u</tspan></text></g>`),
		svgDoc(`<symbol id="s" viewBox="0 0 1 1"><ellipse rx="1" ry="2"/></symbol><use href="#s" width="5" height="5"/>`),
	}
	for _, s := range seeds {
		f.Add([]byte(s), uint8(0))
	}
	f.Fuzz(func(t *testing.T, src []byte, profile uint8) {
		l, err := DecodeSVG(src, DecodeOptions{Profile: Profile(profile % 4), Scale: 1})
		if err != nil {
			if codeOf(err) == 0 {
				t.Fatalf("error without a failure code: %v", err)
			}
			return
		}
		if err := l.Validate(); err != nil {
			t.Fatalf("accepted figure fails validation: %v", err)
		}
		if _, err := DecodeList(EncodeList(l)); err != nil {
			t.Fatalf("accepted figure does not survive the result codec: %v", err)
		}
	})
}
