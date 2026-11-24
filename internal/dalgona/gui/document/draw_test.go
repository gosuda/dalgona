//go:build gui

package document

import (
	"fmt"
	"image"
	"slices"
	"strconv"
	"strings"
	"testing"

	"gioui.org/f32"
	"gioui.org/font"
	"gioui.org/font/gofont"
	"gioui.org/io/system"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/text"
	"gioui.org/unit"
	"golang.org/x/image/math/fixed"
)

var prose = font.Font{Typeface: "Go"}

func testShaper() *text.Shaper {
	return text.NewShaper(text.NoSystemFonts(), text.WithCollection(gofont.Collection()))
}

func testTypography() Typography {
	return Typography{Body: prose, Heading: prose, Mono: prose, Size: 16, CodeSize: 16, Headings: [6]unit.Sp{16, 16, 16, 16, 16, 16}, LineHeight: 1.4, TabSpaces: 4, Indent: 16}
}

func testContext(width int, direction system.TextDirection) layout.Context {
	return layout.Context{
		Ops:         new(op.Ops),
		Metric:      unit.Metric{PxPerDp: 1, PxPerSp: 1},
		Constraints: layout.Exact(image.Pt(width, 4000)),
		Locale:      system.Locale{Language: "en", Direction: direction},
	}
}

func styledParagraph(text string, boundaries ...int) Block {
	var runs []Run
	start, style := 0, Style(0)
	for _, end := range append(boundaries, len(text)) {
		runs = append(runs, Run{Text: Span{Start: start, End: end}, Source: Span{Start: start, End: end}, Kind: Verbatim, Style: style})
		start, style = end, style^Strong
	}
	return Block{Kind: KindParagraph, Projection: Projection{Text: text, Runs: runs}, Rows: []Span{{End: len(text)}}}
}

func shaperLines(shaper *text.Shaper, width int, s string) []string {
	shaper.LayoutString(text.Parameters{Font: prose, PxPerEm: fixed.I(16), MaxWidth: width, WrapPolicy: text.WrapHeuristically, Locale: system.Locale{Language: "en"}}, s)
	var lines []string
	at, start := 0, 0
	for g, ok := shaper.NextGlyph(); ok; g, ok = shaper.NextGlyph() {
		if g.Flags&text.FlagClusterBreak != 0 {
			at = advanceRunes(s, at, len(s), int(g.Runes))
		}
		if g.Flags&text.FlagLineBreak != 0 {
			lines = append(lines, s[start:at])
			start = at
		}
	}
	return lines
}

func TestShapeRowBreaksLinesWhereTheShaperBreaksThem(t *testing.T) {
	shaper := testShaper()
	texts := []string{
		"The quick brown fox jumps over the lazy dog near the riverbank at dawn.",
		"alpha—beta (gamma) delta-epsilon, zeta/eta “theta” iota",
		"supercalifragilisticexpialidocious and antidisestablishmentarianism",
		"한국어 문장은 공백으로 나뉜 어절을 줄 단위로 감쌉니다",
		"日本語の文章は「括弧」や句読点。の前後で改行規則が変わります",
		"see https://example.com/a/very/long/path?with=query&and=more for details",
	}
	for _, s := range texts {
		for _, width := range []int{61, 97, 143, 220} {
			want := shaperLines(shaper, width, s)
			r := ShapeRow(testContext(width, system.LTR), shaper, styledParagraph(s), 0, testTypography())
			if got := lineTexts(r.Flow); !slices.Equal(got, want) {
				t.Errorf("width %d: lines %q, shaper lines %q", width, got, want)
			}
		}
	}
}

type bidiVector struct {
	source string
	styled []int
	rtl    bool
	visual string
}

func codePoints(t *testing.T, hex string) string {
	t.Helper()
	var b strings.Builder
	for field := range strings.FieldsSeq(hex) {
		var r rune
		if _, err := fmt.Sscanf(field, "%x", &r); err != nil {
			t.Fatal(err)
		}
		b.WriteRune(r)
	}
	return b.String()
}

func visualRunes(r *RowLayout) []int {
	var order []int
	for _, i := range r.Order {
		c := r.Clusters[i]
		first := len([]rune(r.Text[:c.Text.Start]))
		count := len([]rune(r.Text[c.Text.Start:c.Text.End]))
		for k := range count {
			if c.RTL {
				order = append(order, first+count-1-k)
			} else {
				order = append(order, first+k)
			}
		}
	}
	return order
}

func TestShapeRowOrdersRunsLikeTheUnicodeBidiVectors(t *testing.T) {
	shaper := testShaper()
	vectors := []bidiVector{
		{source: "05D0 05D1 05D2 0020 0028 0064 0065 0066 0020 0627 0628 062C 0029 0020 0061 0062 0063", visual: "2 1 0 3 4 5 6 7 8 11 10 9 12 13 14 15 16"},
		{source: "0061 0062 0063 0020 0028 0627 0628 062C 0020 0064 0065 0066 0029 0020 05D0 05D1 05D2", visual: "0 1 2 3 4 7 6 5 8 9 10 11 12 13 16 15 14"},
		{source: "05D0 0028 05D1", styled: []int{len("א(")}, visual: "2 1 0"},
		{source: "05D0 0028 05D1 0029", styled: []int{len("א("), len("א(ב")}, visual: "3 2 1 0"},
		{source: "05D0 05D1 0028 05D2 05D3 005B 0026 0065 0066 005D 002E 0029 0067 0068", rtl: true, visual: "12 13 11 10 9 7 8 6 5 4 3 2 1 0"},
	}
	for _, v := range vectors {
		s := codePoints(t, v.source)
		direction := system.LTR
		if v.rtl {
			direction = system.RTL
		}
		r := ShapeRow(testContext(2000, direction), shaper, styledParagraph(s, v.styled...), 0, testTypography())
		want := make([]int, 0, len(s))
		for field := range strings.FieldsSeq(v.visual) {
			n, _ := strconv.Atoi(field)
			want = append(want, n)
		}
		if got := visualRunes(r); len(r.Lines) != 1 || !slices.Equal(got, want) {
			t.Errorf("%s styled at %v: visual order %v in %d lines, want %v", v.source, v.styled, got, len(r.Lines), want)
		}
	}
}

func shapedWidth(shaper *text.Shaper, s string) float32 {
	shaper.LayoutString(text.Parameters{Font: prose, PxPerEm: fixed.I(16), MaxWidth: 1 << 24, Locale: system.Locale{Language: "en"}}, s)
	width := float32(0)
	for g, ok := shaper.NextGlyph(); ok; g, ok = shaper.NextGlyph() {
		width += float32(g.Advance) / 64
	}
	return width
}

func TestInlineFormulaBreaksLikeAPlaceholderOfEqualWidth(t *testing.T) {
	shaper := testShaper()
	width := shapedWidth(shaper, "MMMM")
	formula := markdown(t, "formula", "alpha beta $x^2+y^2$ gamma delta epsilon zeta eta theta").Block(0)
	plain := markdown(t, "plain", "alpha beta MMMM gamma delta epsilon zeta eta theta").Block(0)
	typo := testTypography()
	var asked []string
	typo.Inline = func(b Block, object Run) (InlineBox, bool) {
		asked = append(asked, b.Projection.Text[object.Text.Start:object.Text.End])
		return InlineBox{Width: width, Ascent: 9, Descent: 3}, true
	}
	for w := 60; w <= 320; w += 13 {
		r := ShapeRow(testContext(w, system.LTR), shaper, formula, 0, typo)
		want := lineTexts(ShapeRow(testContext(w, system.LTR), shaper, plain, 0, testTypography()).Flow)
		got := strings.Split(strings.ReplaceAll(strings.Join(lineTexts(r.Flow), "|"), "x^2+y^2", "MMMM"), "|")
		if !slices.Equal(got, want) {
			t.Errorf("width %d: formula lines %q, placeholder lines %q", w, got, want)
		}
		checkBox(t, r.Objects(), InlineBox{Width: width, Ascent: 9, Descent: 3})
	}
	if len(asked) == 0 || asked[0] != "x^2+y^2" {
		t.Fatalf("hook asked for %q", asked)
	}
}

func checkBox(t *testing.T, placed []Placement, box InlineBox) {
	t.Helper()
	if len(placed) != 1 {
		t.Fatalf("placements %+v, want one formula box", placed)
	}
	rect, baseline := placed[0].Rect, placed[0].Baseline
	if rect.X1-rect.X0 != box.Width || baseline-rect.Y0 != box.Ascent || rect.Y1-baseline != box.Descent {
		t.Fatalf("placement %+v, want the reserved box %+v on the baseline", placed[0], box)
	}
}

func TestInlineFormulaHitsAndCaretsAsOneCluster(t *testing.T) {
	shaper := testShaper()
	block := markdown(t, "formula", "a $x^2$ b").Block(0)
	typo := testTypography()
	typo.Inline = func(Block, Run) (InlineBox, bool) { return InlineBox{Width: 40, Ascent: 30, Descent: 6}, true }
	r := ShapeRow(testContext(500, system.LTR), shaper, block, 0, typo)
	placed := r.Objects()
	checkBox(t, placed, InlineBox{Width: 40, Ascent: 30, Descent: 6})
	box, object := placed[0].Rect, placed[0].Text
	if line := r.Lines[0]; placed[0].Baseline-line.Top < 30 {
		t.Fatalf("line %+v does not reserve the box ascent", line)
	}
	middle := (box.Y0 + box.Y1) / 2
	for x := box.X0 + 1; x < box.X1; x += 4 {
		offset, affinity := r.Hit(f32.Pt(x, middle))
		atEdge := offset == object.Start && affinity == After || offset == object.End && affinity == Before
		if !atEdge {
			t.Fatalf("hit at %v inside the formula resolved to %d %v, want an edge of %v", x, offset, affinity, object)
		}
	}
	if caret, _ := r.Caret(object.End, Before); caret.X != box.X1 {
		t.Fatalf("caret after the formula at %v, want %v", caret.X, box.X1)
	}
}
