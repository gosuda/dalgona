package math

import (
	"bytes"
	"strconv"
	"strings"
	"testing"
)

func svgWidthAttribute(t *testing.T, line string) string {
	t.Helper()
	const prefix = `width="`
	start := strings.Index(line, prefix)
	if start < 0 {
		t.Fatalf("exported line %q misses a width attribute", line)
	}
	rest := line[start+len(prefix):]
	end := strings.Index(rest, `"`)
	if end < 0 {
		t.Fatalf("exported width in %q misses its closing quote", line)
	}
	return rest[:end]
}

func checkNonnegativeOuterWidth(t *testing.T, out *Layout) {
	t.Helper()
	if out.Width < 0 {
		t.Fatalf("negative space published width %v, want a nonnegative outer width", out.Width)
	}
}

func checkGlyphsInsideBounds(t *testing.T, out *Layout) {
	t.Helper()
	for _, g := range out.Glyphs {
		if g.X < 0 {
			t.Fatalf("glyph at x %v sits left of the ink origin", g.X)
		}
		if g.X > out.Width {
			t.Fatalf("glyph at x %v escapes width %v", g.X, out.Width)
		}
	}
}

func checkRulesInsideBounds(t *testing.T, out *Layout) {
	t.Helper()
	for _, r := range out.Rules {
		if r.X < 0 || r.X+r.Width > out.Width {
			t.Fatalf("rule %+v escapes width %v", r, out.Width)
		}
	}
}

func TestNegativeSpaceStaysInsideNonnegativeBounds(t *testing.T) {
	layouts := bundledLayouts(t, `\hspace{-10em}x`)
	out := layouts[0]
	t.Run("outerWidth", func(t *testing.T) {
		checkNonnegativeOuterWidth(t, out)
	})
	t.Run("glyphInk", func(t *testing.T) {
		checkGlyphsInsideBounds(t, out)
	})
	t.Run("ruleInk", func(t *testing.T) {
		checkRulesInsideBounds(t, out)
	})
	t.Run("exportedWidth", func(t *testing.T) {
		var svg bytes.Buffer
		if err := out.WriteSVG(&svg); err != nil {
			t.Fatal(err)
		}
		line := strings.SplitN(svg.String(), "\n", 3)[1]
		width := svgWidthAttribute(t, line)
		value, err := strconv.ParseFloat(width, 32)
		if err != nil || value < 0 {
			t.Fatalf("exported width %q, want a nonnegative number", width)
		}
		if !strings.Contains(line, `viewBox="0 0 `) {
			t.Fatalf("exported a shifted viewBox in %q, want the ink origin at zero", line)
		}
	})
}

func TestOrdinaryFormulaKeepsItsMeasuredWidth(t *testing.T) {
	out := bundledLayouts(t, `x+y`)[0]
	if out.Width <= 0 {
		t.Fatalf("ordinary formula width %v, want positive", out.Width)
	}
}
