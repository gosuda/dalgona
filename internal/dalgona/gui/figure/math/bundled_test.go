package math

import (
	"os"
	"path/filepath"
	"slices"
	"testing"
)

func bundledFace(t *testing.T, name string) []byte {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("..", "..", "platform", "fontfaces", name))
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func documentText(t *testing.T) *TextFont {
	t.Helper()
	text, err := NewTextFont(bundledFace(t, "HankenGrotesk-400.ttf"), bundledFace(t, "Pretendard-400.otf"))
	if err != nil {
		t.Fatal(err)
	}
	return text
}

func bundledLayouts(t *testing.T, sources ...string) []*Layout {
	t.Helper()
	font, err := ParseFont(bundledFace(t, "STIXTwoMath-Regular.otf"))
	if err != nil {
		t.Fatal(err)
	}
	text := documentText(t)
	layouts := make([]*Layout, len(sources))
	for i, source := range sources {
		if layouts[i], err = font.Layout(mustParse(t, source), Options{Size: 1000, Text: text}); err != nil {
			t.Fatalf("Layout(%q): %v", source, err)
		}
	}
	return layouts
}

func glyphIDs(out *Layout) []uint32 {
	ids := make([]uint32, len(out.Glyphs))
	for i, glyph := range out.Glyphs {
		ids[i] = glyph.ID
	}
	return ids
}

func TestBundledSTIXTwoMathAppliesItsTables(t *testing.T) {
	cases := []struct {
		source string
		glyphs []uint32
	}{
		{`x^{x^{x}}`, []uint32{3354, 4450, 4699}},
		{`x'`, []uint32{3354, 4515}},
		{`x^2`, []uint32{3354, 4275}},
		{`\hat{x}`, []uint32{3354, 732}},
		{`\hat{b}`, []uint32{3327, 4800}},
		{`\frac{a}{b}`, nil},
	}
	sources := make([]string, len(cases))
	for i, tc := range cases {
		sources[i] = tc.source
	}
	layouts := bundledLayouts(t, sources...)
	for i, tc := range cases[:len(cases)-1] {
		if got := glyphIDs(layouts[i]); !slices.Equal(got, tc.glyphs) {
			t.Errorf("%q draws glyphs %v, want %v", tc.source, got, tc.glyphs)
		}
	}
	rule := onlyRule(t, layouts[len(cases)-1])
	near(t, "STIX fraction rule top", -rule.Y, 258+68/2)
	near(t, "STIX fraction rule thickness", rule.Height, 68)
}

func TestTextFontFallsBackToTheHangulFace(t *testing.T) {
	text := documentText(t)
	metrics := text.MeasureText("a가", 1000)
	near(t, "Hanken a then Pretendard 가 advance", metrics.Width, 540+1770*1000.0/2048)
	near(t, "line ascent", metrics.Ascent, 1000)
	near(t, "line descent", metrics.Descent, 303)
	hangul := outlineBounds(text.AppendOutline(nil, "가", 1000))
	if !(hangul[0] >= 0 && hangul[2] <= 1770*1000.0/2048 && hangul[1] < -500 && hangul[3] > -100) {
		t.Fatalf("가 outline %v is not a Hangul glyph drawn inside its advance above the baseline", hangul)
	}
	if _, err := NewTextFont(); err == nil {
		t.Fatal("NewTextFont accepted no faces")
	}
	if _, err := NewTextFont([]byte("not a font")); err == nil {
		t.Fatal("NewTextFont accepted bytes that are not a font")
	}
}
