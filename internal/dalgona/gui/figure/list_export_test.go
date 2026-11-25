package figure

import (
	"bytes"
	"encoding/base64"
	"encoding/xml"
	"errors"
	"image"
	"image/color"
	"image/png"
	"io"
	"math"
	"strings"
	"testing"

	figmath "github.com/gosuda/dalgona/internal/dalgona/gui/figure/math"
)

func exportRect(x0, y0, x1, y1 float32) Path {
	return Path{
		Verbs:  []Verb{VerbMove, VerbLine, VerbLine, VerbLine, VerbClose},
		Points: []Point{{x0, y0}, {x1, y0}, {x1, y1}, {x0, y1}},
	}
}

func exportTextFace(t *testing.T) figmath.TextFace {
	t.Helper()
	face, err := figmath.NewTextFont(testFonts().Text...)
	if err != nil {
		t.Fatalf("load text faces: %v", err)
	}
	return face
}

func exportSVG(t *testing.T, l *List, face figmath.TextFace) []byte {
	t.Helper()
	var out bytes.Buffer
	if err := WriteSVG(&out, l, face); err != nil {
		t.Fatalf("WriteSVG: %v", err)
	}
	return out.Bytes()
}

func decodeExported(t *testing.T, src []byte) (*List, []Op) {
	t.Helper()
	got, err := DecodeSVG(src, DecodeOptions{Profile: ProfileSVG, Scale: 1})
	if err != nil {
		t.Fatalf("decode exported SVG: %v\n%s", err, src)
	}
	n := len(got.Ops)
	if n < 4 {
		t.Fatalf("decoded figure has %d operations, want the viewport pair around the content", n)
	}
	viewport := exportRect(0, 0, got.Width, got.Height)
	head, tail := got.Ops[:2], got.Ops[n-2:]
	if head[0].Kind != OpClip || !sameExportPath(got.Paths[head[0].Index], viewport) || head[1].Kind != OpTransform || head[1].Transform != Identity() {
		t.Fatalf("decoded figure does not open with the viewport clip and identity viewBox: %+v", head)
	}
	if tail[0].Kind != OpPop || tail[1].Kind != OpPop {
		t.Fatalf("decoded figure does not close the viewport pair: %+v", tail)
	}
	return got, got.Ops[2 : n-2]
}

func sameExportPath(a, b Path) bool {
	if len(a.Verbs) != len(b.Verbs) || len(a.Points) != len(b.Points) {
		return false
	}
	for i := range a.Verbs {
		if a.Verbs[i] != b.Verbs[i] {
			return false
		}
	}
	for i := range a.Points {
		if a.Points[i] != b.Points[i] {
			return false
		}
	}
	return true
}

func exportFixture() *List {
	return &List{
		Width:  200,
		Height: 120,
		Title:  `Round <trip> & "co"`,
		Paths: []Path{
			exportRect(0, 0, 200, 120),
			{
				Verbs:  []Verb{VerbMove, VerbQuad, VerbCube, VerbLine, VerbClose},
				Points: []Point{{10, 10}, {40.5, 0}, {60, 30}, {80, 50}, {90.25, 70}, {70, 90}, {20, 80.125}},
			},
			exportRect(30, 20, 150, 100),
			{Verbs: []Verb{VerbMove, VerbLine, VerbLine, VerbClose}, Points: []Point{{40, 30}, {140, 40}, {90, 95}}},
		},
		Gradients: []Gradient{{
			Start:  Point{40, 30},
			End:    Point{140, 90},
			Stops:  []Stop{{0, Color{255, 0, 0, 255}}, {0.4, Color{0, 128, 0, 255}}, {1, Color{0, 0, 255, 255}}},
			Spread: SpreadReflect,
			Space:  Affine{A: 1, B: 0.25, C: -0.5, D: 1.5, E: 3, F: -2},
		}},
		Ops: []Op{
			{Kind: OpFill, Index: 0, Paint: SolidPaint(Color{16, 32, 48, 255})},
			{Kind: OpTransform, Transform: Affine{A: 0.8, B: 0.1, C: -0.2, D: 1.1, E: 12.5, F: 4}},
			{Kind: OpClip, Index: 2},
			{Kind: OpOpacity, Alpha: 0.625},
			{Kind: OpFill, Index: 1, Paint: SolidPaint(Color{200, 100, 50, 128})},
			{Kind: OpFill, Index: 3, Paint: Paint{Gradient: 0}},
			{Kind: OpPop},
			{Kind: OpPop},
			{Kind: OpPop},
		},
	}
}

func TestExportSVGRoundTripsThroughDecoder(t *testing.T) {
	want := exportFixture()
	src := exportSVG(t, want, nil)
	got, ops := decodeExported(t, src)
	if got.Width != want.Width || got.Height != want.Height || got.Title != want.Title {
		t.Fatalf("decoded size %gx%g title %q, want %gx%g %q", got.Width, got.Height, got.Title, want.Width, want.Height, want.Title)
	}
	if len(ops) != len(want.Ops) {
		t.Fatalf("decoded %d content operations, want %d: %+v", len(ops), len(want.Ops), ops)
	}
	for i, w := range want.Ops {
		compareExportOp(t, i, exportPair{want: want, got: got}, w, ops[i])
	}
}

type exportPair struct{ want, got *List }

func compareExportOp(t *testing.T, i int, lists exportPair, w, g Op) {
	t.Helper()
	if w.Kind != g.Kind {
		t.Fatalf("operation %d is kind %d, want %d", i, g.Kind, w.Kind)
	}
	switch w.Kind {
	case OpTransform:
		if w.Transform != g.Transform {
			t.Fatalf("operation %d transform %+v, want %+v", i, g.Transform, w.Transform)
		}
	case OpOpacity:
		if w.Alpha != g.Alpha {
			t.Fatalf("operation %d opacity %g, want %g", i, g.Alpha, w.Alpha)
		}
	case OpClip, OpFill:
		if !sameExportPath(lists.want.Paths[w.Index], lists.got.Paths[g.Index]) {
			t.Fatalf("operation %d path %+v, want %+v", i, lists.got.Paths[g.Index], lists.want.Paths[w.Index])
		}
		compareExportPaint(t, lists, w, g)
	case OpPop, OpText, OpImage:
	}
}

func compareExportPaint(t *testing.T, lists exportPair, w, g Op) {
	t.Helper()
	if w.Kind != OpFill {
		return
	}
	if (w.Paint.Gradient < 0) != (g.Paint.Gradient < 0) {
		t.Fatalf("fill paint %+v, want %+v", g.Paint, w.Paint)
	}
	if w.Paint.Gradient < 0 {
		if w.Paint.Color != g.Paint.Color {
			t.Fatalf("fill color %+v, want %+v", g.Paint.Color, w.Paint.Color)
		}
		return
	}
	compareExportGradient(t, lists.want.Gradients[w.Paint.Gradient], lists.got.Gradients[g.Paint.Gradient])
}

func compareExportGradient(t *testing.T, w, g Gradient) {
	t.Helper()
	if w.Start != g.Start || w.End != g.End || w.Spread != g.Spread || w.Space != g.Space {
		t.Fatalf("gradient geometry %+v, want %+v", g, w)
	}
	for i := range 41 {
		at := float32(i) / 40
		a, b := sampleExportStops(w.Stops, at), sampleExportStops(g.Stops, at)
		for k := range a {
			if math.Abs(a[k]-b[k]) > 1.5 {
				t.Fatalf("gradient color at %g is %v, want %v", at, b, a)
			}
		}
	}
}

func sampleExportStops(stops []Stop, at float32) [4]float64 {
	prev := stops[0]
	for _, s := range stops {
		if at > s.Offset {
			prev = s
			continue
		}
		f := 0.0
		if s.Offset > prev.Offset {
			f = float64((at - prev.Offset) / (s.Offset - prev.Offset))
		}
		a, b := exportChannels(prev.Color), exportChannels(s.Color)
		for k := range a {
			a[k] += (b[k] - a[k]) * f
		}
		return a
	}
	return exportChannels(prev.Color)
}

func exportChannels(c Color) [4]float64 {
	return [4]float64{float64(c.R), float64(c.G), float64(c.B), float64(c.A)}
}

func TestExportSVGWritesTextAsOutlines(t *testing.T) {
	red, blue := Color{200, 0, 0, 255}, Color{0, 0, 200, 255}
	l := &List{
		Width:  160,
		Height: 80,
		Chunks: []TextChunk{{Origin: Point{120, 50}, Anchor: AnchorEnd, Label: -1, Runs: []TextRun{
			{Text: "HI", Size: 20, Weight: 400, Fill: SolidPaint(red)},
			{Text: "LT", Size: 20, Weight: 700, Italic: true, Face: FaceMono, Fill: SolidPaint(blue), Shift: Point{4, 10}},
		}}},
		Ops: []Op{{Kind: OpText, Index: 0}},
	}
	src := exportSVG(t, l, exportTextFace(t))
	if bytes.Contains(src, []byte("<text")) {
		t.Fatalf("exported SVG keeps a text element:\n%s", src)
	}
	got, ops := decodeExported(t, src)
	if len(got.Chunks) != 0 || len(ops) != 2 || ops[0].Paint != SolidPaint(red) || ops[1].Paint != SolidPaint(blue) {
		t.Fatalf("decoded text export has %d chunks and operations %+v, want two outline fills in the run colors", len(got.Chunks), ops)
	}
	checkExportRunInk(t, exportBounds(got.Paths[ops[0].Index]), exportBounds(got.Paths[ops[1].Index]))
}

func checkExportRunInk(t *testing.T, hi, lt Rect) {
	t.Helper()
	if math.Abs(float64(hi.Max.Y-50)) > 0.05 || math.Abs(float64(lt.Max.Y-60)) > 0.05 {
		t.Fatalf("flat-bottomed glyphs end at %g and %g, want the baselines 50 and 60", hi.Max.Y, lt.Max.Y)
	}
	if lt.Max.X > 120 || lt.Max.X < 117 {
		t.Fatalf("end-anchored chunk ink ends at %g, want just before 120", lt.Max.X)
	}
	if hi.Min.X >= hi.Max.X || lt.Min.X <= hi.Max.X+4 {
		t.Fatalf("run ink %+v then %+v ignores the run order or its 4 unit shift", hi, lt)
	}
}

func exportBounds(p Path) Rect {
	r := Rect{Min: Point{math.MaxFloat32, math.MaxFloat32}, Max: Point{-math.MaxFloat32, -math.MaxFloat32}}
	for _, pt := range p.Points {
		r.Min.X, r.Min.Y = min(r.Min.X, pt.X), min(r.Min.Y, pt.Y)
		r.Max.X, r.Max.Y = max(r.Max.X, pt.X), max(r.Max.Y, pt.Y)
	}
	return r
}

func TestExportSVGEmbedsTilesAsPNGData(t *testing.T) {
	pix := []byte{255, 0, 0, 255, 0, 128, 0, 128, 0, 0, 255, 255, 10, 20, 30, 255}
	l := &List{
		Width:  50,
		Height: 40,
		Tiles:  []Tile{{Bounds: Rect{Min: Point{10, 5}, Max: Point{30, 15}}, Width: 2, Height: 2, Pix: pix}},
		Ops:    []Op{{Kind: OpImage, Index: 0}},
	}
	src := exportSVG(t, l, nil)
	attrs := exportImageAttrs(t, src)
	if attrs["width"] != "2" || attrs["height"] != "2" || attrs["transform"] != "matrix(10 0 0 5 10 5)" || attrs["preserveAspectRatio"] != "none" {
		t.Fatalf("image placement %v, want the 2x2 tile scaled into (10,5)-(30,15)", attrs)
	}
	data, ok := strings.CutPrefix(attrs["href"], "data:image/png;base64,")
	if !ok {
		t.Fatalf("image reference %.40q is not embedded PNG data", attrs["href"])
	}
	img, err := png.Decode(base64.NewDecoder(base64.StdEncoding, strings.NewReader(data)))
	if err != nil {
		t.Fatalf("decode embedded tile: %v", err)
	}
	checkExportPixels(t, img, []color.NRGBA{{255, 0, 0, 255}, {0, 255, 0, 128}, {0, 0, 255, 255}, {10, 20, 30, 255}})
}

func checkExportPixels(t *testing.T, img image.Image, want []color.NRGBA) {
	t.Helper()
	if img.Bounds() != image.Rect(0, 0, 2, 2) {
		t.Fatalf("embedded tile bounds %v, want 2x2", img.Bounds())
	}
	for i, c := range want {
		if got := color.NRGBAModel.Convert(img.At(i%2, i/2)); got != c {
			t.Fatalf("embedded pixel %d is %+v, want %+v", i, got, c)
		}
	}
}

func exportImageAttrs(t *testing.T, src []byte) map[string]string {
	t.Helper()
	dec := xml.NewDecoder(bytes.NewReader(src))
	attrs := map[string]string{}
	images := 0
	for {
		tok, err := dec.Token()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatalf("exported SVG is not well-formed XML: %v\n%s", err, src)
		}
		start, ok := tok.(xml.StartElement)
		if !ok || start.Name.Local != "image" {
			continue
		}
		images++
		for _, a := range start.Attr {
			attrs[a.Name.Local] = a.Value
		}
	}
	if images != 1 {
		t.Fatalf("exported SVG has %d image elements, want 1", images)
	}
	return attrs
}

func exportPNG(t *testing.T, l *List, face figmath.TextFace, scale float32) image.Image {
	t.Helper()
	var out bytes.Buffer
	if err := WritePNG(&out, l, face, scale); err != nil {
		t.Fatalf("WritePNG: %v", err)
	}
	img, err := png.Decode(&out)
	if err != nil {
		t.Fatalf("decode exported PNG: %v", err)
	}
	return img
}

func exportPixel(img image.Image, x, y int) color.NRGBA {
	c, _ := color.NRGBAModel.Convert(img.At(x, y)).(color.NRGBA)
	return c
}

func inExportBox(x, y int, box image.Rectangle) bool { return image.Pt(x, y).In(box) }

func TestExportPNGPlacesFillsAtScale(t *testing.T) {
	l := &List{
		Width:  10.5,
		Height: 7.25,
		Paths:  []Path{exportRect(2, 1, 6, 4), exportRect(0, 0, 5, 10), exportRect(3, 5, 9, 7)},
		Ops: []Op{
			{Kind: OpFill, Index: 0, Paint: SolidPaint(Color{200, 40, 90, 255})},
			{Kind: OpTransform, Transform: Affine{A: 1, D: 1, E: 1}},
			{Kind: OpClip, Index: 1},
			{Kind: OpOpacity, Alpha: 0.5},
			{Kind: OpFill, Index: 2, Paint: SolidPaint(Color{0, 0, 255, 255})},
			{Kind: OpPop},
			{Kind: OpPop},
			{Kind: OpPop},
		},
	}
	img := exportPNG(t, l, nil, 2)
	if img.Bounds() != image.Rect(0, 0, 21, 15) {
		t.Fatalf("PNG bounds %v, want ceil(10.5*2) by ceil(7.25*2) = 21x15", img.Bounds())
	}
	solid, faded := image.Rect(4, 2, 12, 8), image.Rect(8, 10, 12, 14)
	for y := range 15 {
		for x := range 21 {
			want := color.NRGBA{}
			switch {
			case inExportBox(x, y, solid):
				want = color.NRGBA{200, 40, 90, 255}
			case inExportBox(x, y, faded):
				want = color.NRGBA{0, 0, 255, 128}
			}
			if got := exportPixel(img, x, y); got != want {
				t.Fatalf("pixel (%d,%d) is %+v, want %+v", x, y, got, want)
			}
		}
	}
}

var exportTexels = [4]color.NRGBA{{200, 0, 0, 255}, {0, 200, 0, 255}, {0, 0, 200, 255}, {100, 100, 100, 255}}

func TestExportPNGPaintsGradientsTilesAndText(t *testing.T) {
	var pix []byte
	for _, c := range exportTexels {
		pix = append(pix, c.R, c.G, c.B, c.A)
	}
	tile := Tile{Bounds: Rect{Min: Point{1, 9}, Max: Point{3, 11}}, Width: 2, Height: 2, Pix: pix}
	l := &List{
		Width:     40,
		Height:    40,
		Paths:     []Path{exportRect(0, 0, 20, 4)},
		Gradients: []Gradient{{End: Point{10, 0}, Stops: []Stop{{0, Color{255, 0, 0, 255}}, {1, Color{0, 0, 255, 255}}}, Space: Affine{A: 2, D: 1}}},
		Tiles:     []Tile{tile},
		Chunks:    []TextChunk{{Origin: Point{5, 34}, Label: -1, Runs: []TextRun{{Text: "H", Size: 20, Weight: 400, Fill: SolidPaint(Color{0, 90, 0, 255})}}}},
		Ops:       []Op{{Kind: OpFill, Index: 0, Paint: Paint{Gradient: 0}}, {Kind: OpImage, Index: 0}, {Kind: OpText, Index: 0}},
	}
	img := exportPNG(t, l, exportTextFace(t), 2)
	checkExportGradientRow(t, img)
	checkExportTilePixels(t, img)
	checkExportGlyph(t, img, image.Rect(10, 30, 80, 68))
}

func checkExportGradientRow(t *testing.T, img image.Image) {
	t.Helper()
	for x := range 40 {
		f := (float64(x) + 0.5) / 40
		want := color.NRGBA{R: uint8(math.Round(255 * (1 - f))), B: uint8(math.Round(255 * f)), A: 255}
		if got := exportPixel(img, x, 3); exportColorDistance(got, want) > 1 {
			t.Fatalf("gradient pixel (%d,3) is %+v, want %+v", x, got, want)
		}
	}
}

func checkExportTilePixels(t *testing.T, img image.Image) {
	t.Helper()
	for y := 16; y < 26; y++ {
		for x := range 8 {
			want := color.NRGBA{}
			if inExportBox(x, y, image.Rect(2, 18, 6, 22)) {
				want = exportBilinear(exportTexelAxis(x), exportTexelAxis(y-16))
			}
			if got := exportPixel(img, x, y); exportColorDistance(got, want) > 1 {
				t.Fatalf("tile pixel (%d,%d) is %+v, want %+v", x, y, got, want)
			}
		}
	}
}

func exportTexelAxis(device int) float64 {
	return min(max((float64(device)+0.5)/2-1-0.5, 0), 1)
}

func exportBilinear(u, v float64) color.NRGBA {
	var out [4]uint8
	for k := range out {
		ch := func(i int) float64 { return exportChannels(Color(exportTexels[i]))[k] }
		top := ch(0)*(1-u) + ch(1)*u
		bottom := ch(2)*(1-u) + ch(3)*u
		out[k] = uint8(math.Round(top*(1-v) + bottom*v))
	}
	return color.NRGBA{out[0], out[1], out[2], out[3]}
}

func exportColorDistance(a, b color.NRGBA) int {
	d := 0
	for _, pair := range [...][2]uint8{{a.R, b.R}, {a.G, b.G}, {a.B, b.B}, {a.A, b.A}} {
		d = max(d, int(pair[0])-int(pair[1]), int(pair[1])-int(pair[0]))
	}
	return d
}

func checkExportGlyph(t *testing.T, img image.Image, baselineBox image.Rectangle) {
	t.Helper()
	solid := 0
	for y := 24; y < 80; y++ {
		for x := range 80 {
			c := exportPixel(img, x, y)
			if c.A == 0 {
				continue
			}
			if !inExportBox(x, y, baselineBox) {
				t.Fatalf("glyph ink at (%d,%d) lies outside the run box above the baseline %v", x, y, baselineBox)
			}
			if c.A == 255 && c != (color.NRGBA{0, 90, 0, 255}) {
				t.Fatalf("glyph pixel (%d,%d) is %+v, want the run color", x, y, c)
			}
			solid += int(c.A / 255)
		}
	}
	if solid < 40 {
		t.Fatalf("glyph H covers %d solid pixels, want its stems and bar", solid)
	}
}

func expectExportRefusal(t *testing.T, what string, err error, out *bytes.Buffer, fragments ...string) {
	t.Helper()
	if err == nil || out.Len() != 0 {
		t.Fatalf("%s returned %v after writing %d bytes, want a refusal before writing", what, err, out.Len())
	}
	for _, f := range fragments {
		if !strings.Contains(err.Error(), f) {
			t.Fatalf("%s refusal %q does not say %q", what, err, f)
		}
	}
}

func TestExportPNGRefusesOversizedOutput(t *testing.T) {
	var out bytes.Buffer
	large := &List{Width: 1000, Height: 1000}
	expectExportRefusal(t, "36 million pixel export", WritePNG(&out, large, nil, 6), &out, "6000 by 6000", "choose a scale")
	expectExportRefusal(t, "zero scale export", WritePNG(&out, large, nil, 0), &out, "choose a scale")
	img, err := newListImage(exportFixture(), nil, nil, 1)
	if err != nil {
		t.Fatalf("prepare fixture raster: %v", err)
	}
	err = png.Encode(&exportWriter{w: &out, left: 40}, img)
	if !errors.Is(err, errFigureTooLarge) || out.Len() > 40 {
		t.Fatalf("byte-limited export returned %v after writing %d bytes, want a refusal within 40 bytes", err, out.Len())
	}
}

func TestExportRefusesInvalidLists(t *testing.T) {
	var out bytes.Buffer
	broken := &List{Width: 10, Height: 10, Ops: []Op{{Kind: OpFill, Index: 3, Paint: SolidPaint(Color{A: 255})}}}
	expectExportRefusal(t, "SVG export of an invalid list", WriteSVG(&out, broken, nil), &out, "fill path index 3")
	expectExportRefusal(t, "PNG export of an invalid list", WritePNG(&out, broken, nil, 1), &out, "fill path index 3")
	text := &List{Width: 10, Height: 10, Chunks: []TextChunk{{Label: -1, Runs: []TextRun{{Text: "a", Size: 10, Weight: 400, Fill: SolidPaint(Color{A: 255})}}}}, Ops: []Op{{Kind: OpText}}}
	expectExportRefusal(t, "SVG export of text without a face", WriteSVG(&out, text, nil), &out, "text face")
	expectExportRefusal(t, "PNG export of text without a face", WritePNG(&out, text, nil, 1), &out, "text face")
}

type widthFace struct {
	width float32
	marks *[]string
	name  string
}

func (f widthFace) MeasureText(text string, size float32) figmath.TextMetrics {
	return figmath.TextMetrics{Width: float32(len(text)) * f.width * size, Ascent: size}
}

func (f widthFace) AppendOutline(path []figmath.Segment, text string, size float32) []figmath.Segment {
	if f.marks != nil {
		*f.marks = append(*f.marks, f.name)
	}
	return path
}

func TestExportMeasuresMonoRunsWithMonoFace(t *testing.T) {
	runs := []TextRun{{Text: "aaa", Size: 1, Face: FaceSans}, {Text: "bb", Size: 1, Face: FaceMono}}
	single := textPlacer{face: widthFace{width: 1}, mono: widthFace{width: 1}}
	if got := single.measure(runs); got != 5 {
		t.Fatalf("single-face width = %v, want 5", got)
	}
	var marks []string
	p := textPlacer{face: widthFace{width: 1, marks: &marks, name: "sans"}, mono: widthFace{width: 10, marks: &marks, name: "mono"}}
	if got := p.measure(runs); got != 23 {
		t.Fatalf("split-face width = %v, want 3 sans plus 20 mono units", got)
	}
	if err := p.place(TextChunk{Origin: Point{}, Runs: runs}, func(Color, Path) error { return nil }); err != nil {
		t.Fatal(err)
	}
	if len(marks) != 2 || marks[0] != "sans" || marks[1] != "mono" {
		t.Fatalf("outlines used faces %q, want one sans and one mono run in order", marks)
	}
}
