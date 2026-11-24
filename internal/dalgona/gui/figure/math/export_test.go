package math

import (
	"bytes"
	"encoding/xml"
	"errors"
	"image"
	"image/png"
	"io"
	stdmath "math"
	"strconv"
	"strings"
	"testing"
)

type svgElement struct {
	ID        string `xml:"id,attr"`
	Href      string `xml:"http://www.w3.org/1999/xlink href,attr"`
	Transform string `xml:"transform,attr"`
	D         string `xml:"d,attr"`
}

type svgDocument struct {
	Width   string       `xml:"width,attr"`
	Height  string       `xml:"height,attr"`
	ViewBox string       `xml:"viewBox,attr"`
	Defs    []svgElement `xml:"defs>path"`
	Uses    []svgElement `xml:"use"`
	Paths   []svgElement `xml:"path"`
}

func svgNumbers(t *testing.T, s string) []float32 {
	t.Helper()
	var values []float32
	for _, field := range strings.FieldsFunc(s, func(r rune) bool { return r == ' ' || r == '(' || r == ')' }) {
		if field == "matrix" || len(field) == 1 && strings.ContainsAny(field, "MLQCZ") {
			continue
		}
		v, err := strconv.ParseFloat(field, 32)
		if err != nil {
			t.Fatalf("SVG number %q: %v", field, err)
		}
		values = append(values, float32(v))
	}
	return values
}

func svgBounds(t *testing.T, d string, matrix []float32) [4]float32 {
	t.Helper()
	values := svgNumbers(t, d)
	bounds := [4]float32{float32(stdmath.Inf(1)), float32(stdmath.Inf(1)), float32(stdmath.Inf(-1)), float32(stdmath.Inf(-1))}
	for i := 0; i+1 < len(values); i += 2 {
		x := matrix[0]*values[i] + matrix[2]*values[i+1] + matrix[4]
		y := matrix[1]*values[i] + matrix[3]*values[i+1] + matrix[5]
		bounds = [4]float32{min(bounds[0], x), min(bounds[1], y), max(bounds[2], x), max(bounds[3], y)}
	}
	return bounds
}

var identity = []float32{1, 0, 0, 1, 0, 0}

func nearBox(t *testing.T, label string, got, want [4]float32) {
	t.Helper()
	for i := range want {
		near(t, label, got[i], want[i])
	}
}

func exportSVG(t *testing.T, out *Layout) svgDocument {
	t.Helper()
	var buf bytes.Buffer
	if err := out.WriteSVG(&buf); err != nil {
		t.Fatal(err)
	}
	var doc svgDocument
	if err := xml.Unmarshal(buf.Bytes(), &doc); err != nil {
		t.Fatalf("exported SVG does not parse: %v\n%s", err, buf.Bytes())
	}
	return doc
}

func glyphDefinitions(t *testing.T, doc svgDocument, placements int) map[string]string {
	t.Helper()
	defs := make(map[string]string, len(doc.Defs))
	for _, def := range doc.Defs {
		defs["#"+def.ID] = def.D
	}
	if len(doc.Defs) != len(defs) || len(defs) >= placements || len(doc.Uses) != placements {
		t.Fatalf("%d glyph definitions with %d identities and %d uses for %d placements; each glyph needs one definition", len(doc.Defs), len(defs), len(doc.Uses), placements)
	}
	return defs
}

func TestSVGExportReproducesTheLayoutBoxes(t *testing.T) {
	out := render(t, `\frac{a}{y}+\sqrt{x}\text{ab}+a`, false)
	doc := exportSVG(t, out)
	height := out.Ascent + out.Descent
	near(t, "SVG width", svgNumbers(t, doc.Width)[0], out.Width)
	near(t, "SVG height", svgNumbers(t, doc.Height)[0], height)
	nearBox(t, "SVG view box", [4]float32(svgNumbers(t, doc.ViewBox)), [4]float32{0, 0, out.Width, height})
	defs := glyphDefinitions(t, doc, len(out.Glyphs))
	for i, use := range doc.Uses {
		nearBox(t, "glyph ink", svgBounds(t, defs[use.Href], svgNumbers(t, use.Transform)), inkBox(out.Glyphs[i], out.Ascent))
	}
	if len(doc.Paths) != len(out.Rules)+len(out.Texts) {
		t.Fatalf("SVG has %d paths, want %d rules and %d text runs", len(doc.Paths), len(out.Rules), len(out.Texts))
	}
	for i, rule := range out.Rules {
		nearBox(t, "rule", svgBounds(t, doc.Paths[i].D, identity), [4]float32{rule.X, out.Ascent + rule.Y, rule.X + rule.Width, out.Ascent + rule.Y + rule.Height})
	}
	run := out.Texts[0]
	nearBox(t, "text run", svgBounds(t, doc.Paths[len(out.Rules)].D, identity), [4]float32{run.X, out.Ascent + run.Y - 0.7*run.Size, run.X + run.Size, out.Ascent + run.Y + 0.2*run.Size})
}

type failingWriter struct{ err error }

func (w failingWriter) Write([]byte) (int, error) { return 0, w.err }

func TestSVGExportReportsWriteFailures(t *testing.T) {
	broken := errors.New("disk full")
	if err := render(t, `a+b`, false).WriteSVG(failingWriter{broken}); !errors.Is(err, broken) {
		t.Fatalf("WriteSVG = %v, want the write failure", err)
	}
}

func inkedArea(img image.Image) image.Rectangle {
	var area image.Rectangle
	for y := img.Bounds().Min.Y; y < img.Bounds().Max.Y; y++ {
		for x := img.Bounds().Min.X; x < img.Bounds().Max.X; x++ {
			if _, _, _, a := img.At(x, y).RGBA(); a > 0 {
				area = area.Union(image.Rect(x, y, x+1, y+1))
			}
		}
	}
	return area
}

func expectedInk(out *Layout, scale float32) image.Rectangle {
	var area image.Rectangle
	add := func(box [4]float32) {
		area = area.Union(image.Rect(
			int(stdmath.Floor(float64(box[0]*scale))), int(stdmath.Floor(float64(box[1]*scale))),
			int(stdmath.Ceil(float64(box[2]*scale))), int(stdmath.Ceil(float64(box[3]*scale)))))
	}
	for _, glyph := range out.Glyphs {
		add(inkBox(glyph, out.Ascent))
	}
	for _, rule := range out.Rules {
		add([4]float32{rule.X, out.Ascent + rule.Y, rule.X + rule.Width, out.Ascent + rule.Y + rule.Height})
	}
	return area
}

func exportPNG(t *testing.T, out *Layout, scale float32) image.Image {
	t.Helper()
	var buf bytes.Buffer
	if err := out.WritePNG(&buf, scale); err != nil {
		t.Fatal(err)
	}
	img, err := png.Decode(&buf)
	if err != nil {
		t.Fatal(err)
	}
	return img
}

func TestPNGExportInksTheLayoutAcrossBandsAndTiles(t *testing.T) {
	out := render(t, `\sqrt{\begin{matrix}1\\2\\3\\4\end{matrix}}+\frac{a}{y}+\overline{xxxxxxxxxx}`, false)
	const scale = 0.12
	img := exportPNG(t, out, scale)
	bounds := img.Bounds()
	want := image.Rect(0, 0, int(stdmath.Ceil(float64(out.Width*scale))), int(stdmath.Ceil(float64((out.Ascent+out.Descent)*scale))))
	if bounds != want || bounds.Dx() <= rasterTile || bounds.Dy() <= rasterTile {
		t.Fatalf("PNG is %v, want %v crossing a band and a tile boundary", bounds, want)
	}
	if got, want := inkedArea(img), expectedInk(out, scale); !within(got, want, 1) {
		t.Fatalf("inked pixels cover %v, layout ink covers %v", got, want)
	}
	rule := ruleOfThickness(t, out, c(fractionRuleThickness))
	x, y := int((rule.X+rule.Width/2)*scale), int((out.Ascent+rule.Y+rule.Height/2)*scale)
	gap := int((out.Ascent + (rule.Y+only(t, out, "a").Y)/2) * scale)
	if _, _, _, a := img.At(x, y).RGBA(); a != 0xffff {
		t.Fatalf("fraction rule center (%d,%d) has alpha %#x, want opaque", x, y, a)
	}
	if _, _, _, a := img.At(x, gap).RGBA(); a != 0 {
		t.Fatalf("the gap between numerator and rule at (%d,%d) has alpha %#x", x, gap, a)
	}
	assertGlyphCentersInked(t, img, out, scale)
	assertRulesInked(t, img, out, scale)
}

func assertRulesInked(t *testing.T, img image.Image, out *Layout, scale float32) {
	t.Helper()
	for _, rule := range out.Rules {
		y := int((out.Ascent + rule.Y + rule.Height/2) * scale)
		for x := int(rule.X*scale) + 2; x < int((rule.X+rule.Width)*scale)-2; x += 8 {
			if _, _, _, a := img.At(x, y).RGBA(); a == 0 {
				t.Fatalf("rule %+v is blank at (%d,%d)", rule, x, y)
			}
		}
	}
}

func TestRasterClosesOpenContours(t *testing.T) {
	r := &raster{scale: 1, bounds: image.Rect(0, 0, 30, 12)}
	r.band = image.Alpha{Pix: make([]byte, 30*12), Stride: 30, Rect: r.bounds}
	var path []Segment
	path = appendRectangle(path, 0, 0, 10, 10)
	path = appendRectangle(path, 20, 2, 30, 12)
	r.fill(path)
	for y := range 12 {
		for x := range 30 {
			inside := image.Pt(x, y).In(image.Rect(0, 0, 10, 10)) || image.Pt(x, y).In(image.Rect(20, 2, 30, 12))
			if got := r.band.AlphaAt(x, y).A; inside != (got == 0xff) || !inside && got != 0 {
				t.Fatalf("pixel (%d,%d) alpha %d, inside %v", x, y, got, inside)
			}
		}
	}
}

func assertGlyphCentersInked(t *testing.T, img image.Image, out *Layout, scale float32) {
	t.Helper()
	for _, glyph := range out.Glyphs {
		box := inkBox(glyph, out.Ascent)
		x, y := int((box[0]+box[2])/2*scale), int((box[1]+box[3])/2*scale)
		if _, _, _, a := img.At(x, y).RGBA(); a == 0 {
			t.Errorf("glyph %d ink center (%d,%d) is blank", glyph.ID, x, y)
		}
	}
}

func within(got, want image.Rectangle, slack int) bool {
	return abs(got.Min.X-want.Min.X) <= slack && abs(got.Min.Y-want.Min.Y) <= slack &&
		abs(got.Max.X-want.Max.X) <= slack && abs(got.Max.Y-want.Max.Y) <= slack
}

func abs(v int) int {
	return max(v, -v)
}

func ruleOfThickness(t *testing.T, out *Layout, thickness float32) Rule {
	t.Helper()
	for _, rule := range out.Rules {
		if stdmath.Abs(float64(rule.Height-thickness)) < 1e-2 {
			return rule
		}
	}
	t.Fatalf("layout has no rule %v thick: %+v", thickness, out.Rules)
	return Rule{}
}

func TestPNGExportRefusesUnboundedRasters(t *testing.T) {
	out := render(t, `a+b`, false)
	for _, scale := range []float32{0, -1, float32(stdmath.NaN()), float32(stdmath.Inf(1)), 1e4} {
		if err := out.WritePNG(io.Discard, scale); err == nil || !strings.Contains(err.Error(), "pixels") {
			t.Errorf("WritePNG at scale %v = %v, want a pixel bound refusal", scale, err)
		}
	}
	limited := &limitedWriter{w: io.Discard, left: 4}
	if _, err := limited.Write([]byte("12345")); !errors.Is(err, errExportTooLarge) {
		t.Fatalf("write beyond the byte budget = %v", err)
	}
}
