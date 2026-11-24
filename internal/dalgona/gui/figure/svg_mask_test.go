package figure

import (
	"math"
	"strings"
	"testing"
)

type sample struct{ r, g, b, a float64 }

func (s sample) over(dst sample) sample {
	keep := 1 - s.a
	return sample{s.r + dst.r*keep, s.g + dst.g*keep, s.b + dst.b*keep, s.a + dst.a*keep}
}

type sampleLayer struct {
	space   matrix
	inside  bool
	group   bool
	opacity float64
	content sample
}

func inverseWinding(p Path, space matrix, x, y float32) int {
	inverse, ok := space.invert()
	if !ok {
		return 0
	}
	at := inverse.vec(vec{float64(x), float64(y)})
	return windingAt(p, float32(at.x), float32(at.y))
}

func composite(l *List, x, y float32) sample {
	stack := []sampleLayer{{space: identity, inside: true, opacity: 1}}
	for _, o := range l.Ops {
		top := &stack[len(stack)-1]
		switch o.Kind {
		case OpTransform, OpClip, OpOpacity:
			stack = append(stack, top.child(l, o, x, y))
		case OpPop:
			stack = popLayer(stack)
		case OpFill:
			if top.inside && inverseWinding(l.Paths[o.Index], top.space, x, y) != 0 {
				top.content = colorSample(o.Paint.Color).over(top.content)
			}
		case OpText, OpImage:
		}
	}
	return stack[0].content
}

func (s sampleLayer) child(l *List, o Op, x, y float32) sampleLayer {
	switch o.Kind {
	case OpOpacity:
		return sampleLayer{space: s.space, inside: s.inside, group: true, opacity: float64(o.Alpha)}
	case OpClip:
		s.inside = s.inside && inverseWinding(l.Paths[o.Index], s.space, x, y) != 0
	case OpTransform:
		s.space = s.space.mul(matrixOf(o.Transform))
	case OpPop, OpFill, OpText, OpImage:
	}
	s.group = false
	return s
}

func popLayer(stack []sampleLayer) []sampleLayer {
	popped := stack[len(stack)-1]
	stack = stack[:len(stack)-1]
	parent := &stack[len(stack)-1]
	if !popped.group {
		parent.content = popped.content
		return stack
	}
	c, k := popped.content, popped.opacity
	parent.content = sample{c.r * k, c.g * k, c.b * k, c.a * k}.over(parent.content)
	return stack
}

func colorSample(c Color) sample {
	a := float64(c.A) / 255
	return sample{float64(c.R) / 255 * a, float64(c.G) / 255 * a, float64(c.B) / 255 * a, a}
}

func coveredCount(l *List, width, height int) int {
	count := 0
	for y := range height {
		for x := range width {
			if composite(l, float32(x)+0.5, float32(y)+0.5).a > 0.5 {
				count++
			}
		}
	}
	return count
}

const maskedSquare = `<rect width="100" height="100" fill="black" mask="url(#m)"/>`

func TestDecodeMaskRevealsOverlappingShapesInPaintOrder(t *testing.T) {
	l := decodeRaw(t, svgDoc(`<mask id="m" maskUnits="userSpaceOnUse" x="0" y="0" width="100" height="100">`+
		`<rect width="100" height="100" fill="white"/><rect x="10" y="10" width="30" height="30" fill="black"/>`+
		`<rect x="30" y="30" width="20" height="20" fill="white"/><rect x="60" y="60" width="20" height="20" fill="black"/>`+
		`<rect x="70" y="70" width="20" height="20" fill="black"/></mask>`+maskedSquare))
	if got := coveredCount(l, 100, 100); got != 10000-900+100-700 {
		t.Fatalf("revealed pixels = %d, want 8500", got)
	}
	for _, p := range [...]struct {
		x, y  float32
		alpha float64
	}{{15, 15, 0}, {35, 35, 1}, {45, 45, 1}, {65, 65, 0}, {85, 85, 0}, {95, 5, 1}} {
		if got := composite(l, p.x, p.y).a; math.Abs(got-p.alpha) > 1e-6 {
			t.Errorf("alpha at (%g, %g) = %g, want %g", p.x, p.y, got, p.alpha)
		}
	}
}

func TestDecodeMaskWeighsPartialLuminanceAndAlpha(t *testing.T) {
	cases := []struct {
		name string
		mask string
		want float64
	}{
		{"grey luminance", `<mask id="m"><rect width="100" height="100" fill="#808080"/></mask>`, 128.0 / 255},
		{"coloured luminance", `<mask id="m"><rect width="100" height="100" fill="#ff0000"/></mask>`, 0.2125},
		{"translucent white", `<mask id="m"><rect width="100" height="100" fill="white" fill-opacity="0.25"/></mask>`, 0.25},
		{"alpha type", `<mask id="m" mask-type="alpha"><rect width="100" height="100" fill="#ff0000" opacity="0.5"/></mask>`, 0.5},
		{"translucent overlap", `<mask id="m"><rect width="100" height="100" fill="white" fill-opacity="0.5"/><rect width="100" height="100" fill="white" fill-opacity="0.5"/></mask>`, 0.75},
	}
	for _, c := range cases {
		l := decodeRaw(t, svgDoc(c.mask+maskedSquare))
		if got := composite(l, 50, 50).a; math.Abs(got-c.want) > 1.0/255 {
			t.Errorf("%s: alpha = %g, want %g", c.name, got, c.want)
		}
		if got := composite(l, 150, 50).a; got != 0 {
			t.Errorf("%s: alpha outside the masked shape = %g, want 0", c.name, got)
		}
	}
}

func TestDecodeMaskFollowsGradients(t *testing.T) {
	linear := decodeRaw(t, svgDoc(`<linearGradient id="g" gradientUnits="userSpaceOnUse" x1="0" x2="100"><stop offset="0" stop-color="white"/><stop offset="1" stop-color="black"/></linearGradient>`+
		`<mask id="m"><rect width="100" height="100" fill="url(#g)"/></mask>`+maskedSquare))
	for _, x := range [...]float32{5, 25, 50, 75, 95} {
		want := 1 - float64(x)/100
		if got := composite(linear, x, 50).a; math.Abs(got-want) > 0.02 {
			t.Errorf("linear gradient mask alpha at x=%g = %g, want %g", x, got, want)
		}
	}
	total := 0.0
	for y := range 100 {
		for x := range 100 {
			total += composite(linear, float32(x)+0.5, float32(y)+0.5).a
		}
	}
	if math.Abs(total-5000) > 50 {
		t.Errorf("linear gradient mask coverage = %g, want 5000", total)
	}
	radial := decodeRaw(t, svgDoc(`<radialGradient id="g" gradientUnits="userSpaceOnUse" cx="50" cy="50" r="50"><stop offset="0" stop-color="white"/><stop offset="1" stop-color="black"/></radialGradient>`+
		`<mask id="m"><rect width="100" height="100" fill="url(#g)"/></mask>`+maskedSquare))
	for _, p := range [...]struct{ x, y, want float32 }{{50, 50, 1}, {75, 50, 0.5}, {50, 90, 0.2}, {3, 3, 0}} {
		if got := composite(radial, p.x, p.y).a; math.Abs(got-float64(p.want)) > 0.03 {
			t.Errorf("radial gradient mask alpha at (%g, %g) = %g, want %g", p.x, p.y, got, p.want)
		}
	}
}

func TestDecodeClipUnitesEvenOddChildren(t *testing.T) {
	l := decodeRaw(t, svgDoc(`<clipPath id="c"><path clip-rule="evenodd" d="M0 0 H40 V40 H0 Z M10 10 H30 V30 H10 Z"/><rect x="50" y="50" width="10" height="10"/></clipPath>`+
		`<rect width="200" height="100" fill="black" clip-path="url(#c)"/>`))
	if got := coveredCount(l, 100, 100); got != 40*40-20*20+10*10 {
		t.Fatalf("clipped pixels = %d, want 1300", got)
	}
	if composite(l, 20, 20).a != 0 || composite(l, 5, 20).a != 1 {
		t.Fatal("evenodd clip child must exclude its hole and keep its ring")
	}
	l = decodeRaw(t, svgDoc(`<clipPath id="c"><path clip-rule="evenodd" d="M0 0 H40 V40 H0 Z M10 10 H30 V30 H10 Z"/><rect x="15" y="15" width="10" height="10"/></clipPath>`+
		`<rect width="200" height="100" fill="black" clip-path="url(#c)"/>`))
	if got := coveredCount(l, 100, 100); got != 40*40-20*20+10*10 {
		t.Fatalf("clipped pixels with a child inside the hole = %d, want 1300", got)
	}
}

func TestDecodeMultiplyBlendDarkensItsBackdrop(t *testing.T) {
	src := svgDoc(`<rect width="200" height="100" fill="#ffffff"/><rect y="40" width="200" height="20" fill="#0000ff"/>` +
		`<g style="mix-blend-mode: multiply; opacity: 0.5"><rect x="40" width="20" height="100" fill="#ff0000"/></g>` +
		`<rect x="45" y="80" width="10" height="10" fill="#00ff00"/>`)
	l, err := DecodeSVG([]byte(src), DecodeOptions{Profile: ProfileD2, Scale: 1})
	if err != nil {
		t.Fatal(err)
	}
	if err = l.Validate(); err != nil {
		t.Fatal(err)
	}
	for _, p := range [...]struct {
		x, y    float32
		r, g, b float64
	}{
		{50, 50, 0, 0, 0.5},
		{50, 10, 1, 0.5, 0.5},
		{10, 50, 0, 0, 1},
		{10, 10, 1, 1, 1},
		{50, 85, 0, 1, 0},
	} {
		got := composite(l, p.x, p.y)
		if math.Abs(got.r-p.r) > 1.0/255 || math.Abs(got.g-p.g) > 1.0/255 || math.Abs(got.b-p.b) > 1.0/255 || math.Abs(got.a-1) > 1.0/255 {
			t.Errorf("colour at (%g, %g) = %+v, want %g %g %g", p.x, p.y, got, p.r, p.g, p.b)
		}
	}
}

func TestDecodeMultiplyBlendRefusesText(t *testing.T) {
	_, err := DecodeSVG([]byte(svgDoc(`<rect width="10" height="10"/><g style="mix-blend-mode: multiply"><text>t</text></g>`)), DecodeOptions{Profile: ProfileD2, Scale: 1})
	if f := FailureOf(err); err == nil || !strings.Contains(f.Message, "multiply") || !f.Span.Known() {
		t.Fatalf("multiply blend of text = %v, want a located refusal", err)
	}
}
