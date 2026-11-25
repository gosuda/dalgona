//go:build gui

package platform

import (
	"slices"
	"testing"

	giofont "gioui.org/font"
	"gioui.org/font/opentype"
	"gioui.org/text"
	"golang.org/x/image/math/fixed"

	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

func bundledShaper(t *testing.T) *text.Shaper {
	t.Helper()
	faces, err := FontCollection()
	if err != nil {
		t.Fatal(err)
	}
	return text.NewShaper(text.NoSystemFonts(), text.WithCollection(faces))
}

func shapeGlyphs(s *text.Shaper, face giofont.Typeface, weight theme.Weight, str string) []text.Glyph {
	s.LayoutString(text.Parameters{
		Font:     giofont.Font{Typeface: face, Weight: FontWeight(weight)},
		PxPerEm:  fixed.I(16),
		MaxWidth: 1 << 20,
	}, str)
	var out []text.Glyph
	for g, ok := s.NextGlyph(); ok; g, ok = s.NextGlyph() {
		out = append(out, g)
	}
	return out
}

func advance(glyphs []text.Glyph) fixed.Int26_6 {
	var sum fixed.Int26_6
	for _, g := range glyphs {
		sum += g.Advance
	}
	return sum
}

func TestEachRegisteredWeightSelectsItsOwnHeavierFace(t *testing.T) {
	s := bundledShaper(t)
	ladders := []struct {
		face    giofont.Typeface
		weights []theme.Weight
	}{
		{TitleFace, []theme.Weight{theme.WeightTitle, theme.WeightControl, theme.WeightStrong}},
		{ProseFace, []theme.Weight{theme.WeightText, theme.WeightTitle, theme.WeightStrong}},
	}
	for _, ladder := range ladders {
		seen := map[text.GlyphID]theme.Weight{}
		var lighter fixed.Int26_6
		for _, w := range ladder.weights {
			glyphs := shapeGlyphs(s, ladder.face, w, "Dalgona working")
			if prev, dup := seen[glyphs[0].ID]; dup {
				t.Errorf("%s weight %d reuses the face of weight %d", ladder.face, w, prev)
			}
			seen[glyphs[0].ID] = w
			width := advance(glyphs)
			if width <= lighter {
				t.Errorf("%s weight %d is %v wide, not wider than the lighter weight at %v", ladder.face, w, width, lighter)
			}
			lighter = width
		}
	}
}

func decoyShaper(t *testing.T) *text.Shaper {
	t.Helper()
	faces, err := FontCollection()
	if err != nil {
		t.Fatal(err)
	}
	decoy, err := opentype.Parse(pretendard700)
	if err != nil {
		t.Fatal(err)
	}
	decoyFace := giofont.FontFace{Font: giofont.Font{Typeface: "Decoy Hangul", Weight: FontWeight(theme.WeightTitle)}, Face: decoy}
	return text.NewShaper(text.NoSystemFonts(), text.WithCollection(append([]giofont.FontFace{decoyFace}, faces...)))
}

func glyphIDs(glyphs []text.Glyph) []text.GlyphID {
	ids := make([]text.GlyphID, len(glyphs))
	for i, g := range glyphs {
		ids[i] = g.ID
	}
	return ids
}

func TestHangulFollowsThePretendardFallback(t *testing.T) {
	s := decoyShaper(t)
	want := glyphIDs(shapeGlyphs(s, "Pretendard", theme.WeightTitle, "달고나"))
	if distinct := slices.Compact(slices.Sorted(slices.Values(want))); len(distinct) != 3 {
		t.Fatalf("Pretendard shapes 달고나 as %d distinct glyphs, want 3", len(distinct))
	}
	for _, face := range []giofont.Typeface{TitleFace, ProseFace} {
		if got := glyphIDs(shapeGlyphs(s, face, theme.WeightTitle, "달고나")); !slices.Equal(got, want) {
			t.Errorf("%s draws 달고나 from another face than Pretendard 600", face)
		}
	}
}

func TestHangulWeightsUseTheirOwnPretendardFace(t *testing.T) {
	s := decoyShaper(t)
	weights := map[text.GlyphID]theme.Weight{}
	for _, w := range []theme.Weight{theme.WeightText, theme.WeightTitle, theme.WeightStrong} {
		id := shapeGlyphs(s, ProseFace, w, "달")[0].ID
		if prev, dup := weights[id]; dup {
			t.Errorf("Hangul weight %d reuses the Pretendard face of weight %d", w, prev)
		}
		weights[id] = w
	}
}

func TestIconFaceDrawsRegisteredGlyphs(t *testing.T) {
	s := bundledShaper(t)
	glyphs := shapeGlyphs(s, IconFace, theme.WeightText, "\U000F0874\U000F0E47\U000F0256")
	distinct := map[text.GlyphID]bool{}
	for _, g := range glyphs {
		distinct[g.ID] = true
	}
	if len(distinct) != 3 {
		t.Fatalf("icon face draws %d distinct shapes for three registered glyphs, want 3", len(distinct))
	}
}
