package figure

import (
	"math"
	"strings"
	"testing"
)

func runsWith(l *List, keep func(TextRun) bool) []TextRun {
	var out []TextRun
	for _, c := range l.Chunks {
		for _, r := range c.Runs {
			if keep(r) {
				out = append(out, r)
			}
		}
	}
	return out
}

func chunkOf(l *List, text string) (TextChunk, bool) {
	for _, c := range l.Chunks {
		for _, r := range c.Runs {
			if strings.Contains(r.Text, text) {
				return c, true
			}
		}
	}
	return TextChunk{}, false
}

var markdownRunChecks = []struct {
	name string
	keep func(TextRun) bool
}{
	{"heading at twice the body size", func(r TextRun) bool { return strings.Contains(r.Text, "Title") && r.Size == 32 && r.Weight == 700 }},
	{"strong run", func(r TextRun) bool { return r.Text == "bold" && r.Weight == 700 && !r.Italic }},
	{"emphasis run", func(r TextRun) bool { return r.Text == "slanted" && r.Italic }},
	{"inline code run", func(r TextRun) bool { return r.Text == "code" && r.Face == FaceMono }},
	{"list bullets", func(r TextRun) bool { return r.Text == "•" }},
	{"edge emphasis", func(r TextRun) bool { return r.Text == "edge" && r.Italic }},
}

func TestD2RendersMarkdownLabelsNatively(t *testing.T) {
	source := "doc: |md\n# Title 제목\nSome **bold** and *slanted* words with `code`\n\n- one\n- two\n|\ndoc -> end: |md *edge*|\nend\n"
	l, err := renderWith(t, &engines{}, KindD2, source, testTheme)
	if err != nil {
		t.Fatal(err)
	}
	if err := l.Validate(); err != nil {
		t.Fatal(err)
	}
	requireLabels(t, l, "Title 제목", "bold", "slanted", "code", "one", "two", "edge")
	for _, c := range markdownRunChecks {
		if len(runsWith(l, c.keep)) == 0 {
			t.Errorf("display list lacks the %s", c.name)
		}
	}
	if got := len(runsWith(l, func(r TextRun) bool { return r.Text == "•" })); got != 2 {
		t.Errorf("list bullets = %d, want 2", got)
	}
	title, _ := chunkOf(l, "Title")
	body, _ := chunkOf(l, "bold")
	first, _ := chunkOf(l, "one")
	if !(title.Origin.Y < body.Origin.Y && body.Origin.Y < first.Origin.Y) {
		t.Errorf("block baselines %g, %g, %g do not descend in source order", title.Origin.Y, body.Origin.Y, first.Origin.Y)
	}
	requireChunksInsideFigure(t, l)
}

func requireChunksInsideFigure(t *testing.T, l *List) {
	t.Helper()
	for _, c := range l.Chunks {
		if c.Origin.X < 0 || c.Origin.Y < 0 || c.Origin.X > l.Width || c.Origin.Y > l.Height {
			t.Errorf("label chunk at %+v lies outside the %gx%g figure", c.Origin, l.Width, l.Height)
		}
	}
}

func TestD2RendersLaTeXLabelsAsMathOutlines(t *testing.T) {
	l, err := renderWith(t, newEngines(testFonts()), KindD2, "f: |latex\n\\frac{a}{b}\n|\n", testTheme)
	if err != nil {
		t.Fatal(err)
	}
	if err := l.Validate(); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(strings.Join(l.Labels, "|"), `\frac{a}{b}`) {
		t.Fatalf("labels %q lack the formula source", l.Labels)
	}
	bar, glyphs := fractionParts(l)
	if bar.Width() == 0 {
		t.Fatal("formula has no fraction bar")
	}
	above, below := glyphsAroundBar(bar, glyphs)
	if above != 1 || below != 1 {
		t.Fatalf("glyphs above and below the fraction bar = %d and %d, want 1 and 1", above, below)
	}
}

func glyphsAroundBar(bar Rect, glyphs []Rect) (int, int) {
	above, below := 0, 0
	for _, g := range glyphs {
		mid := (g.Min.X + g.Max.X) / 2
		if mid < bar.Min.X || mid > bar.Max.X {
			continue
		}
		switch {
		case g.Max.Y <= bar.Min.Y:
			above++
		case g.Min.Y >= bar.Max.Y:
			below++
		}
	}
	return above, below
}

func fractionParts(l *List) (Rect, []Rect) {
	var bar Rect
	var glyphs []Rect
	for _, o := range opsOf(l, OpFill) {
		r := pathBounds(l.Paths[o.Index])
		if r.Width() > 4*r.Height() && r.Height() < 3 {
			bar = r
			continue
		}
		if r.Width() < 40 && r.Height() < 40 {
			glyphs = append(glyphs, r)
		}
	}
	return bar, glyphs
}

func sampleMatches(s sample, want [3]float64) bool {
	return math.Abs(s.r-want[0]) < 1.5/255 && math.Abs(s.g-want[1]) < 1.5/255 && math.Abs(s.b-want[2]) < 1.5/255
}

func TestD2SequenceGroupMultipliesItsBackdrop(t *testing.T) {
	theme := testTheme
	theme.Canvas, theme.Tone, theme.Surface = Color{0xC0, 0xC0, 0xC0, 0xFF}, Color{0x80, 0x40, 0xC0, 0xFF}, Color{0x40, 0xC0, 0x40, 0xFF}
	l, err := renderWith(t, &engines{}, KindD2, "shape: sequence_diagram\nalice -> bob: hi\ng: grp {\n  alice -> bob: inside\n}\n", theme)
	if err != nil {
		t.Fatal(err)
	}
	canvas, tone := theme.Canvas, theme.Tone
	multiply := func(c uint8, s uint8) float64 { return float64(c) / 255 * (1 - 0.5*(1-float64(s)/255)) }
	normal := func(c uint8, s uint8) float64 { return 0.5*float64(c)/255 + 0.5*float64(s)/255 }
	wantMultiply := [3]float64{multiply(canvas.R, tone.R), multiply(canvas.G, tone.G), multiply(canvas.B, tone.B)}
	wantNormal := [3]float64{normal(canvas.R, tone.R), normal(canvas.G, tone.G), normal(canvas.B, tone.B)}
	multiplied, blended := 0, 0
	for y := float32(1); y < l.Height; y += 6 {
		for x := float32(1); x < l.Width; x += 6 {
			s := composite(l, x, y)
			if sampleMatches(s, wantMultiply) {
				multiplied++
			}
			if sampleMatches(s, wantNormal) {
				blended++
			}
		}
	}
	if multiplied == 0 || blended != 0 {
		t.Fatalf("group samples: %d multiplied, %d normally blended; want only multiplied", multiplied, blended)
	}
}
