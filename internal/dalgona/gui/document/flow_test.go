package document

import (
	"slices"
	"strings"
	"testing"
	"unicode/utf8"
)

func uniform(text string, advance float32, rtl bool) []Cluster {
	var clusters []Cluster
	for at := 0; at < len(text); {
		_, width := utf8.DecodeRuneInString(text[at:])
		clusters = append(clusters, Cluster{Text: Span{Start: at, End: at + width}, Advance: advance, Ascent: 8, Descent: 2, RTL: rtl})
		at += width
	}
	return clusters
}

func breakable(clusters []Cluster, ends ...int) []Cluster {
	for i := range clusters {
		clusters[i].Break = slices.Contains(ends, clusters[i].Text.End)
	}
	return clusters
}

func afterSpaces(text string) []int {
	var ends []int
	for i, r := range text {
		if r == ' ' {
			ends = append(ends, i+1)
		}
	}
	return ends
}

func lineTexts(f Flow) []string {
	texts := make([]string, len(f.Lines))
	for i, line := range f.Lines {
		texts[i] = f.Text[line.Text.Start:line.Text.End]
	}
	return texts
}

func TestArrangeBreaksAtOpportunitiesAndOverlongWords(t *testing.T) {
	cases := []struct {
		text  string
		ends  []int
		lines []string
	}{
		{"alpha beta gamma", afterSpaces("alpha beta gamma"), []string{"alpha beta ", "gamma"}},
		{"alpha beta gamma", nil, []string{"alpha beta", " gamma"}},
		{"aaaaaaaaaaaaaaa", nil, []string{"aaaaaaaaaa", "aaaaa"}},
		{"one\ntwo\n", nil, []string{"one\n", "two\n"}},
		{"a\r\nb", nil, []string{"a\r\n", "b"}},
		{"", nil, []string{""}},
		{"abcdefgh  ij", []int{len("abcdefgh  ")}, []string{"abcdefgh  ", "ij"}},
		{"abcdefghij  k", []int{len("abcdefghij  ")}, []string{"abcdefghij", "  k"}},
	}
	for _, c := range cases {
		clusters := uniform(c.text, 10, false)
		if strings.Contains(c.text, "\r\n") {
			clusters = append(clusters[:1], Cluster{Text: Span{Start: 1, End: 3}, Ascent: 8, Descent: 2}, clusters[3])
		}
		f := Arrange(c.text, Span{End: len(c.text)}, breakable(clusters, c.ends...), Measure{MaxWidth: 100, LineHeight: 16})
		if got := lineTexts(f); !slices.Equal(got, c.lines) {
			t.Errorf("%q with breaks after %v: lines %q, want %q", c.text, c.ends, got, c.lines)
		}
	}
	wide := "日本語日本語"
	f := Arrange(wide, Span{End: len(wide)}, breakable(uniform(wide, 30, false), 3, 6, 9, 12, 15), Measure{MaxWidth: 100, LineHeight: 16})
	if got := lineTexts(f); !slices.Equal(got, []string{"日本語", "日本語"}) {
		t.Fatalf("ideographs %q", got)
	}
	if f.Height != 32 || f.Lines[1].Top != 16 || f.Lines[0].Baseline != 3+8 {
		t.Fatalf("line geometry %+v height %v", f.Lines, f.Height)
	}
}

func TestArrangeAdvancesTabsToStops(t *testing.T) {
	text := "a\tb"
	f := Arrange(text, Span{End: len(text)}, uniform(text, 10, false), Measure{MaxWidth: 500, LineHeight: 16, Tab: func(x float32) float32 { return (float32(int(x/40)) + 1) * 40 }})
	if f.X[2] != 40 {
		t.Fatalf("text after a tab starts at %v", f.X[2])
	}
}

func ligature() Flow {
	text := "fiab"
	clusters := []Cluster{
		{Text: Span{Start: 0, End: 2}, Advance: 20, Ascent: 8, Descent: 2},
		{Text: Span{Start: 2, End: 3}, Advance: 10, Ascent: 8, Descent: 2},
		{Text: Span{Start: 3, End: 4}, Advance: 10, Ascent: 8, Descent: 2},
	}
	return Arrange(text, Span{End: len(text)}, clusters, Measure{MaxWidth: 500, LineHeight: 16})
}

func TestHitUsesClusterGeometryAndAffinity(t *testing.T) {
	f := ligature()
	cases := []struct {
		x        float32
		offset   int
		affinity Affinity
	}{
		{-5, 0, After}, {4, 0, After}, {7, 1, Before}, {12, 1, After}, {18, 2, Before}, {24, 2, After}, {29, 3, Before}, {99, 4, Before},
	}
	for _, c := range cases {
		offset, affinity := f.Hit(c.x, 5)
		if offset != c.offset || affinity != c.affinity {
			t.Errorf("hit at %v: %d %v, want %d %v", c.x, offset, affinity, c.offset, c.affinity)
		}
	}
}

func TestCaretAndHighlightSplitALigatureByGraphemes(t *testing.T) {
	f := ligature()
	if x, top, bottom := f.Caret(1, After); x != 10 || top != 0 || bottom != 16 {
		t.Fatalf("caret inside the ligature at %v %v %v", x, top, bottom)
	}
	rects := f.Highlight(Span{Start: 1, End: 3})
	if len(rects) != 1 || rects[0].X0 != 10 || rects[0].X1 != 30 {
		t.Fatalf("highlight %+v", rects)
	}
}

func TestHitInRightToLeftRunMapsToLogicalEdges(t *testing.T) {
	text := "שלום"
	f := Arrange(text, Span{End: len(text)}, uniform(text, 10, true), Measure{MaxWidth: 500, LineHeight: 16})
	if f.X[0] != 30 || f.X[3] != 0 {
		t.Fatalf("visual positions %v", f.X)
	}
	if offset, affinity := f.Hit(38, 5); offset != 0 || affinity != After {
		t.Fatalf("right edge of the first letter hit %d %v", offset, affinity)
	}
	if offset, affinity := f.Hit(32, 5); offset != len("ש") || affinity != Before {
		t.Fatalf("left half of the first letter hit %d %v", offset, affinity)
	}
	if x, _, _ := f.Caret(0, After); x != 40 {
		t.Fatalf("caret before the first letter at %v", x)
	}
}

func TestHitBelowTheLastLineEndsBeforeTheNewline(t *testing.T) {
	text := "ab\n"
	f := Arrange(text, Span{End: len(text)}, uniform(text, 10, false), Measure{MaxWidth: 500, LineHeight: 16})
	if len(f.Lines) != 1 {
		t.Fatalf("trailing newline produced %d lines", len(f.Lines))
	}
	if offset, affinity := f.Hit(200, 40); offset != 2 || affinity != Before {
		t.Fatalf("hit past the end %d %v", offset, affinity)
	}
}

func mixedDirection() (string, []Cluster) {
	text := "ab אב cd"
	clusters := uniform(text, 10, false)
	for i := range clusters {
		clusters[i].RTL = i >= 3 && i <= 4
	}
	return text, breakable(clusters, afterSpaces(text)...)
}

func TestCaretFollowsAffinityAtSoftBreaksAndDirectionBoundaries(t *testing.T) {
	text, clusters := mixedDirection()
	f := Arrange(text, Span{End: len(text)}, clusters, Measure{MaxWidth: 500, LineHeight: 16})
	if got := f.Order; !slices.Equal(got, []int{0, 1, 2, 4, 3, 5, 6, 7}) {
		t.Fatalf("visual order %v", got)
	}
	hebrew := strings.Index(text, "א")
	cases := []struct {
		offset   int
		affinity Affinity
		x        float32
	}{
		{0, After, 0}, {hebrew, Before, 30}, {hebrew, After, 50}, {hebrew + len("אב"), Before, 30}, {hebrew + len("אב"), After, 50}, {len(text), Before, 80},
	}
	for _, c := range cases {
		if x, _, _ := f.Caret(c.offset, c.affinity); x != c.x {
			t.Errorf("caret at %d %v: x %v, want %v", c.offset, c.affinity, x, c.x)
		}
	}

	wrapped := Arrange(text, Span{End: len(text)}, clusters, Measure{MaxWidth: 35, LineHeight: 16})
	if got := lineTexts(wrapped); !slices.Equal(got, []string{"ab ", "אב ", "cd"}) {
		t.Fatalf("wrapped lines %q", got)
	}
	soft := strings.Index(text, "א")
	if x, top, _ := wrapped.Caret(soft, Before); x != 30 || top != 0 {
		t.Fatalf("caret before a soft break at %v,%v; want the end of the first line", x, top)
	}
	if x, top, _ := wrapped.Caret(soft, After); x != 20 || top != 16 {
		t.Fatalf("caret after a soft break at %v,%v; want the right edge of the leading right-to-left letter", x, top)
	}
}

func TestHitThenCaretStaysOnTheHitCluster(t *testing.T) {
	text, clusters := mixedDirection()
	f := Arrange(text, Span{End: len(text)}, clusters, Measure{MaxWidth: 500, LineHeight: 16})
	for x := float32(0.5); x < 80; x++ {
		offset, affinity := f.Hit(x, 5)
		caret, _, _ := f.Caret(offset, affinity)
		cell := float32(int(x/10) * 10)
		if caret != cell && caret != cell+10 {
			t.Errorf("hit at %v resolved to %d %v, whose caret %v leaves the hit cluster [%v,%v]", x, offset, affinity, caret, cell, cell+10)
		}
	}
}

func TestRightToLeftParagraphReversesTheLine(t *testing.T) {
	text, clusters := mixedDirection()
	for i := range clusters {
		clusters[i].RTL = i >= 2 && i <= 5
	}
	f := Arrange(text, Span{End: len(text)}, clusters, Measure{MaxWidth: 500, LineHeight: 16, RTL: true})
	if got := f.Order; !slices.Equal(got, []int{6, 7, 5, 4, 3, 2, 0, 1}) {
		t.Fatalf("right-to-left paragraph visual order %v", got)
	}
}
