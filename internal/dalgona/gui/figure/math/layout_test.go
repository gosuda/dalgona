package math

import (
	"cmp"
	"errors"
	stdmath "math"
	"slices"
	"strings"
	"testing"
	"time"
	"unicode/utf8"
)

const em = 1000

type halfEmText struct{}

func (halfEmText) MeasureText(text string, size float32) TextMetrics {
	return TextMetrics{Width: float32(utf8.RuneCountInString(text)) * size / 2, Ascent: size * 0.7, Descent: size * 0.2}
}

func (halfEmText) AppendOutline(path []Segment, text string, size float32) []Segment {
	var x float32
	for range utf8.RuneCountInString(text) {
		path = appendRectangle(path, x, -0.7*size, x+size/2, 0.2*size)
		x += size / 2
	}
	return path
}

func render(t *testing.T, source string, display bool) *Layout {
	t.Helper()
	out, err := testFont(t).Layout(mustParse(t, source), Options{Size: em, Display: display, Text: halfEmText{}})
	if err != nil {
		t.Fatalf("Layout(%q): %v", source, err)
	}
	return out
}

func c(id constant) float32 { return fixtureConstant(id) }

func ink(name string) [4]float32 {
	for _, glyph := range fixtureGlyphs() {
		if glyph.name == name {
			return [4]float32{float32(glyph.ink[0]), float32(glyph.ink[1]), float32(glyph.ink[2]), float32(glyph.ink[3])}
		}
	}
	panic("fixture has no glyph " + name)
}

func glyphsNamed(out *Layout, name string) []Glyph {
	var found []Glyph
	for _, glyph := range out.Glyphs {
		if glyph.ID == gid(name) {
			found = append(found, glyph)
		}
	}
	return found
}

func only(t *testing.T, out *Layout, name string) Glyph {
	t.Helper()
	found := glyphsNamed(out, name)
	if len(found) != 1 {
		t.Fatalf("layout has %d %q glyphs, want 1: %+v", len(found), name, out.Glyphs)
	}
	return found[0]
}

func near(t *testing.T, label string, got, want float32) {
	t.Helper()
	if stdmath.Abs(float64(got-want)) > 1e-2 {
		t.Errorf("%s = %.3f, want %.3f", label, got, want)
	}
}

func atLeast(t *testing.T, label string, got, least float32) {
	t.Helper()
	if got < least-1e-2 {
		t.Errorf("%s = %.3f, want at least %.3f", label, got, least)
	}
}

func onlyRule(t *testing.T, out *Layout) Rule {
	t.Helper()
	if len(out.Rules) != 1 {
		t.Fatalf("layout has %d rules, want 1: %+v", len(out.Rules), out.Rules)
	}
	return out.Rules[0]
}

const script = float32(fixtureScriptPercent) / 100

func TestFractionPlacesNumeratorAndDenominatorFromMathConstants(t *testing.T) {
	axis, thickness := c(axisHeight), c(fractionRuleThickness)
	cases := []struct {
		source, numerator, denominator string
		display                        bool
		scale                          float32
	}{
		{`\frac{a}{b}`, "a", "b", false, script},
		{`\frac{y}{b}`, "y", "b", false, script},
		{`\frac{a}{(}`, "a", "(", false, script},
		{`\frac{a}{b}`, "a", "b", true, 1},
		{`\dfrac{y}{b}`, "y", "b", false, 1},
	}
	for _, tc := range cases {
		out := render(t, tc.source, tc.display)
		shiftUp, gapUp := c(fractionNumeratorShiftUp), c(fractionNumeratorGapMin)
		shiftDown, gapDown := c(fractionDenominatorShiftDown), c(fractionDenominatorGapMin)
		if tc.scale == 1 {
			shiftUp, gapUp = c(fractionNumeratorDisplayStyleShiftUp), c(fractionNumDisplayStyleGapMin)
			shiftDown, gapDown = c(fractionDenominatorDisplayStyleShiftDown), c(fractionDenomDisplayStyleGapMin)
		}
		numeratorDepth := -ink(tc.numerator)[1] * tc.scale
		denominatorHeight := ink(tc.denominator)[3] * tc.scale
		up := max(shiftUp, axis+thickness/2+gapUp+numeratorDepth)
		down := max(shiftDown, gapDown+thickness/2-axis+denominatorHeight)
		near(t, tc.source+" numerator baseline", -only(t, out, tc.numerator).Y, up)
		near(t, tc.source+" denominator baseline", only(t, out, tc.denominator).Y, down)
		rule := onlyRule(t, out)
		near(t, tc.source+" rule top", -rule.Y, axis+thickness/2)
		near(t, tc.source+" rule thickness", rule.Height, thickness)
		near(t, tc.source+" rule start", rule.X, nullDelimiterSpace*em)
		atLeast(t, tc.source+" numerator gap", up-numeratorDepth-(axis+thickness/2), gapUp)
		atLeast(t, tc.source+" denominator gap", (axis-thickness/2)-(denominatorHeight-down), gapDown)
	}
}

func TestBinomialStacksWithoutRuleAndKeepsTheStackGap(t *testing.T) {
	out := render(t, `\binom{a}{b}`, false)
	if len(out.Rules) != 0 {
		t.Fatalf("binom drew rules %+v", out.Rules)
	}
	near(t, "binom numerator", -only(t, out, "a").Y, c(stackTopShiftUp))
	near(t, "binom denominator", only(t, out, "b").Y, c(stackBottomShiftDown))
	if len(glyphsNamed(out, "paren.v1")) != 1 || len(glyphsNamed(out, ")")) != 1 {
		t.Fatalf("binom lacks its parentheses: %+v", out.Glyphs)
	}

	crowded := render(t, `\binom{y}{\sqrt{b}}`, false)
	numeratorBottom := -only(t, crowded, "y").Y - (-ink("y")[1] * script)
	radicalRule := onlyRule(t, crowded)
	denominatorTop := -radicalRule.Y + c(radicalExtraAscender)*script
	near(t, "crowded binom gap", numeratorBottom-denominatorTop, c(stackGapMin))
}

func TestScriptsUseMathConstants(t *testing.T) {
	digit := ink("2")[3] * script
	cases := []struct {
		source, glyph string
		x, shift      float32
	}{
		{`x^2`, "2", 500, max(c(superscriptShiftUp), c(superscriptBottomMin))},
		{`x^y`, "y", 500, max(c(superscriptShiftUp), -ink("y")[1]*script+c(superscriptBottomMin))},
		{`x_2`, "2", 500, -max(c(subscriptShiftDown), digit-c(subscriptTopMax))},
		{`x_{(}`, "(", 500, -max(c(subscriptShiftDown), ink("(")[3]*script-c(subscriptTopMax))},
		{`{b}^2`, "2", 500, max(ink("b")[3]-c(superscriptBaselineDropMax), c(superscriptShiftUp))},
		{`{y}_2`, "2", 500, -max(-ink("y")[1]+c(subscriptBaselineDropMin), c(subscriptShiftDown))},
		{`(^2`, "2", 400, max(ink("(")[3]-c(superscriptBaselineDropMax), c(superscriptShiftUp))},
		{`)^2`, "2", 400, max(c(superscriptShiftUp), c(superscriptBottomMin))},
		{`f_2`, "2", 500, -max(c(subscriptShiftDown), digit-c(subscriptTopMax))},
	}
	for _, tc := range cases {
		glyph := only(t, render(t, tc.source, false), tc.glyph)
		near(t, tc.source+" script x", glyph.X, tc.x)
		near(t, tc.source+" script shift", -glyph.Y, tc.shift)
	}
	cramped := only(t, render(t, `\sqrt{x^2}`, false), "2")
	near(t, "cramped superscript shift", -cramped.Y, c(superscriptShiftUpCramped))
	near(t, "script advance", render(t, `x^2`, false).Width, 500+500*script+c(spaceAfterScript))
}

func TestSubscriptAndSuperscriptKeepTheirGap(t *testing.T) {
	out := render(t, `x_2^3`, false)
	digit := ink("2")[3] * script
	sup, sub := -only(t, out, "3").Y, only(t, out, "2").Y
	near(t, "script gap", sup-(digit-sub), c(subSuperscriptGapMin))
	supStart := max(c(superscriptShiftUp), c(superscriptBottomMin))
	subStart := max(c(subscriptShiftDown), digit-c(subscriptTopMax))
	deficit := c(subSuperscriptGapMin) - (supStart - (digit - subStart))
	raise := min(deficit, c(superscriptBottomMaxWithSubscript)-supStart)
	near(t, "superscript raise", sup, supStart+raise)
	near(t, "subscript drop", sub, subStart+deficit-raise)
}

func TestScriptsUseItalicCorrectionAndMathKerning(t *testing.T) {
	sup := only(t, render(t, `f^2`, false), "2")
	shift := max(c(superscriptShiftUp), c(superscriptBottomMin))
	kern := min(float32(-30), float32(-80))
	if ink("f")[3] < 400 || shift >= 400 {
		t.Fatalf("fixture no longer straddles the kern height")
	}
	near(t, "kerned superscript x", sup.X, 500+100+kern)
	sub := only(t, render(t, `f_2`, false), "2")
	near(t, "subscript x ignores italic correction", sub.X, 500)
	prime := only(t, render(t, `f'`, false), "′")
	near(t, "prime x", prime.X, 500+100+kern)
	near(t, "prime shift", -prime.Y, c(superscriptShiftUp))
	integral := render(t, `\int_0^1`, false)
	near(t, "integral script offset", only(t, integral, "1").X-only(t, integral, "0").X, 150)
}

func TestLargeOperatorsUseDisplayVariantAndLimits(t *testing.T) {
	out := render(t, `\sum_i^n`, true)
	operator := only(t, out, "sum.display")
	box := ink("sum.display")
	shift := (box[3]+box[1])/2 - c(axisHeight)
	near(t, "display operator axis shift", operator.Y, shift)
	top, bottom := box[3]-shift, -box[1]+shift
	upper, lower := only(t, out, "n"), only(t, out, "i")
	near(t, "upper limit baseline", -upper.Y, top+max(c(upperLimitBaselineRiseMin), c(upperLimitGapMin)))
	near(t, "lower limit baseline", lower.Y, bottom+max(c(lowerLimitBaselineDropMin), c(lowerLimitGapMin)+ink("i")[3]*script))
	near(t, "upper limit centered", upper.X, (1400-500*script)/2)
	atLeast(t, "upper limit gap", -upper.Y-top, c(upperLimitGapMin))
	if len(glyphsNamed(out, "∑")) != 0 {
		t.Fatalf("display sum kept the text glyph")
	}

	inline := render(t, `\sum_i^n`, false)
	only(t, inline, "∑")
	near(t, "inline superscript beside operator", only(t, inline, "n").X, 1000)
	near(t, "inline superscript drop", -only(t, inline, "n").Y, max(750-c(superscriptBaselineDropMax), c(superscriptShiftUp)))

	beside := render(t, `\sum\nolimits_i`, true)
	near(t, "nolimits subscript x", only(t, beside, "i").X, 1400)
	above := render(t, `\int\limits_0^1`, false)
	near(t, "limits upper italic offset", only(t, above, "1").X, (500-500*script+150)/2)
	near(t, "limits lower italic offset", only(t, above, "0").X, (500-500*script-150)/2)
	function := render(t, `\lim_x`, true)
	near(t, "function limit drop", only(t, function, "x").Y, max(c(lowerLimitBaselineDropMin), c(lowerLimitGapMin)+450*script))
}

func TestRadicalUsesMathConstants(t *testing.T) {
	for _, tc := range []struct {
		display bool
		gap     float32
	}{{false, c(radicalVerticalGap)}, {true, c(radicalDisplayStyleVerticalGap)}} {
		out := render(t, `\sqrt{x}`, tc.display)
		thickness := c(radicalRuleThickness)
		target := 450 + tc.gap + thickness
		gap := tc.gap + (1200-target)/2
		rule := onlyRule(t, out)
		near(t, "radical rule bottom", -rule.Y-rule.Height, 450+gap)
		near(t, "radical rule thickness", rule.Height, thickness)
		near(t, "radical rule start", rule.X, 800)
		near(t, "radical extra ascender", out.Ascent-(-rule.Y), c(radicalExtraAscender))
		sign := only(t, out, "√")
		near(t, "radical sign top meets rule top", -sign.Y+ink("√")[3], -rule.Y)
		atLeast(t, "radical gap", -rule.Y-rule.Height-450, tc.gap)
	}
}

func TestRadicalDegreeUsesKernsAndRaise(t *testing.T) {
	out := render(t, `\sqrt[3]{x}`, false)
	degree := only(t, out, "3")
	small := float32(fixtureScriptScriptPercent) / 100
	signX := c(radicalKernBeforeDegree) + 500*small + c(radicalKernAfterDegree)
	offset := max(0, -signX)
	near(t, "degree x", degree.X, c(radicalKernBeforeDegree)+offset)
	sign := only(t, out, "√")
	near(t, "sign x", sign.X, signX+offset)
	signBottom := -sign.Y + ink("√")[1]
	near(t, "degree baseline", -degree.Y, signBottom+fixtureDegreeRaisePercent*1200/100)
}

func assemblyParts(out *Layout, names ...string) []Glyph {
	var parts []Glyph
	for _, name := range names {
		parts = append(parts, glyphsNamed(out, name)...)
	}
	return parts
}

func verticalSpan(parts []Glyph, inkOf func(Glyph) [4]float32) (float32, float32) {
	bottom, top := float32(stdmath.Inf(1)), float32(stdmath.Inf(-1))
	for _, part := range parts {
		box := inkOf(part)
		bottom = min(bottom, -part.Y+box[1])
		top = max(top, -part.Y+box[3])
	}
	return bottom, top
}

func inkByID(glyph Glyph) [4]float32 {
	for name, id := range buildFixture().glyphs {
		if id == glyph.ID {
			return ink(name)
		}
	}
	panic("unknown glyph")
}

func TestTallDelimiterUsesGlyphAssemblyCenteredOnAxis(t *testing.T) {
	out := render(t, `\left( \begin{matrix}1\\2\\3\end{matrix} \right)`, false)
	parts := assemblyParts(out, "paren.bottom", "paren.extender", "paren.top")
	if len(glyphsNamed(out, "paren.bottom")) != 1 || len(glyphsNamed(out, "paren.top")) != 1 || len(glyphsNamed(out, "paren.extender")) != 7 {
		t.Fatalf("assembly parts %+v", out.Glyphs)
	}
	target := float32(3 * em * (strutHeightEm + strutDepthEm))
	bottom, top := verticalSpan(parts, inkByID)
	atLeast(t, "assembly height", top-bottom, target)
	near(t, "assembly centered on axis", (top+bottom)/2, c(axisHeight))
	for _, glyph := range out.Glyphs {
		if glyph.Size != em {
			t.Fatalf("glyph %d drawn at size %v; delimiters must not scale", glyph.ID, glyph.Size)
		}
	}
	assertOverlaps(t, parts, 50, 200)
	if len(glyphsNamed(out, ")")) != 1 {
		t.Fatalf("closing parenthesis without a construction must keep its glyph")
	}
}

func assertOverlaps(t *testing.T, parts []Glyph, least, most float32) {
	t.Helper()
	spans := make([][2]float32, len(parts))
	for i, part := range parts {
		box := inkByID(part)
		spans[i] = [2]float32{-part.Y + box[1], -part.Y + box[3]}
	}
	slices.SortFunc(spans, func(a, b [2]float32) int { return cmp.Compare(a[0], b[0]) })
	for i := 1; i < len(spans); i++ {
		overlap := spans[i-1][1] - spans[i][0]
		if overlap < least-1e-2 || overlap > most+1e-2 {
			t.Errorf("parts %d and %d overlap by %.2f, want %v to %v", i-1, i, overlap, least, most)
		}
	}
}

func TestDelimiterVariantsFollowTheTargetSize(t *testing.T) {
	small := render(t, `\left( x \right)`, false)
	near(t, "small parenthesis on axis", only(t, small, "(").Y, 0)
	medium := render(t, `\left( \frac{a}{b} \right)`, false)
	only(t, medium, "paren.v1")
	sized := render(t, `\big(`, false)
	only(t, sized, "paren.v1")
	huge := render(t, `\Bigg(`, false)
	parts := assemblyParts(huge, "paren.bottom", "paren.extender", "paren.top")
	bottom, top := verticalSpan(parts, inkByID)
	atLeast(t, "Bigg height", top-bottom, 3*em)
	null := render(t, `\left. x \right)`, false)
	near(t, "null delimiter space", only(t, null, "x").X, nullDelimiterSpace*em)
	middle := render(t, `\left( a \middle( b \right.`, false)
	if len(glyphsNamed(middle, "(")) != 2 {
		t.Fatalf("middle delimiter missing: %+v", middle.Glyphs)
	}
}

func TestRadicalAssemblyCoversTallRadicand(t *testing.T) {
	out := render(t, `\sqrt{\begin{matrix}1\\2\\3\end{matrix}}`, false)
	parts := assemblyParts(out, "radical.bottom", "radical.extender", "radical.top")
	if len(glyphsNamed(out, "radical.extender")) != 5 {
		t.Fatalf("radical assembly parts %+v", out.Glyphs)
	}
	bottom, top := verticalSpan(parts, inkByID)
	half := float32(3*em*(strutHeightEm+strutDepthEm)) / 2
	target := 2*half + c(radicalVerticalGap) + c(radicalRuleThickness)
	atLeast(t, "radical assembly height", top-bottom, target)
	rule := onlyRule(t, out)
	near(t, "radical rule meets assembly top", -rule.Y, top)
	atLeast(t, "tall radical gap", -rule.Y-rule.Height-(half+c(axisHeight)), c(radicalVerticalGap))
}

func TestAccentsUseAttachmentAndAccentBaseHeight(t *testing.T) {
	x := only(t, render(t, `\hat{x}`, false), "hat")
	near(t, "hat over x", x.X, 300-(-250))
	near(t, "hat over x height", x.Y, 0)
	digit := only(t, render(t, `\hat{2}`, false), "hat")
	near(t, "hat over 2", digit.X, 250-(-250))
	near(t, "hat over 2 raise", -digit.Y, 650-c(accentBaseHeight))
	wide := only(t, render(t, `\widehat{xy}`, false), "hat.wide")
	near(t, "wide hat meets its attachment at the base center", wide.X, 0)

	over := render(t, `\overline{x}`, false)
	bar := onlyRule(t, over)
	near(t, "overbar bottom", -bar.Y-bar.Height, 450+c(overbarVerticalGap))
	near(t, "overbar thickness", bar.Height, c(overbarRuleThickness))
	near(t, "overbar ascender", over.Ascent, 450+c(overbarVerticalGap)+c(overbarRuleThickness)+c(overbarExtraAscender))
	under := render(t, `\underline{y}`, false)
	bar = onlyRule(t, under)
	near(t, "underbar top", bar.Y, 200+c(underbarVerticalGap))
	near(t, "underbar thickness", bar.Height, c(underbarRuleThickness))
	near(t, "underbar descender", under.Descent, 200+c(underbarVerticalGap)+c(underbarRuleThickness)+c(underbarExtraDescender))
}

func TestBracesStretchAndCarryLabels(t *testing.T) {
	brace := only(t, render(t, `\overbrace{xyz}`, false), "brace.wide")
	near(t, "overbrace gap", -brace.Y+ink("brace.wide")[1]-450, c(stretchStackGapBelowMin))

	wide := render(t, `\overbrace{xyzw}`, false)
	parts := assemblyParts(wide, "brace.left", "brace.extender", "brace.middle", "brace.right")
	if len(glyphsNamed(wide, "brace.extender")) != 4 {
		t.Fatalf("brace assembly %+v", wide.Glyphs)
	}
	left, right := float32(stdmath.Inf(1)), float32(stdmath.Inf(-1))
	for _, part := range parts {
		box := inkByID(part)
		left, right = min(left, part.X+box[0]), max(right, part.X+box[2])
	}
	atLeast(t, "brace assembly width", right-left, 2000)

	labeled := render(t, `\overbrace{xyz}^{n}`, false)
	braceTop := -only(t, labeled, "brace.wide").Y + ink("brace.wide")[3]
	near(t, "overbrace label", -only(t, labeled, "n").Y, braceTop+max(c(stretchStackTopShiftUp), c(stretchStackGapAboveMin)))

	under := render(t, `\underbrace{xyz}_{n}`, false)
	mark := only(t, under, "⏟")
	near(t, "underbrace gap", ink("y")[1]-(-mark.Y+ink("⏟")[3]), c(stretchStackGapAboveMin))
	braceBottom := mark.Y - ink("⏟")[1]
	near(t, "underbrace label", only(t, under, "n").Y, braceBottom+max(c(stretchStackBottomShiftDown), c(stretchStackGapBelowMin)+450*script))
}

func TestAtomSpacingFollowsTheTeXTable(t *testing.T) {
	mu := float32(em) / 18
	cases := []struct {
		source, glyph string
		x             float32
	}{
		{`a+b`, "+", 500 + 4*mu},
		{`a+b`, "b", 500 + 4*mu + 800 + 4*mu},
		{`+b`, "b", 800},
		{`a=b`, "=", 500 + 5*mu},
		{`a,b`, "b", 500 + 300 + 3*mu},
		{`\sin x`, "x", 1500 + 3*mu},
		{`x^{a+b}`, "b", 500 + (500+800)*script},
		{`a\quad b`, "b", 500 + 18*mu},
		{`a\!b`, "b", 500 - 3*mu},
		{`a+=b`, "=", 500 + 800 + 5*mu},
	}
	for _, tc := range cases {
		near(t, tc.source+" "+tc.glyph, only(t, render(t, tc.source, false), tc.glyph).X, tc.x)
	}
}

func TestArraysPlaceRowsColumnsAndTags(t *testing.T) {
	out := render(t, `\begin{matrix}a&b\\c&d\end{matrix}`, false)
	rowHeight, rowDepth := float32(strutHeightEm*em), float32(strutDepthEm*em)
	top := (2*(rowHeight+rowDepth))/2 + c(axisHeight)
	near(t, "first row baseline", -only(t, out, "a").Y, top-rowHeight)
	near(t, "second row baseline", -only(t, out, "c").Y, top-rowHeight-rowDepth-rowHeight)
	near(t, "second column", only(t, out, "b").X, 500+em)

	aligned := render(t, `\begin{aligned}a&=b\end{aligned}`, false)
	near(t, "relation after alignment marker", only(t, aligned, "=").X, 500+5*float32(em)/18)

	array := render(t, `\begin{array}{c|c}a&b\end{array}`, false)
	rule := onlyRule(t, array)
	near(t, "array rule", rule.X+rule.Width/2, (500+500+(500+500+em))/2)
	near(t, "array rule height", rule.Height, rowHeight+rowDepth)

	tagged := render(t, `\begin{align}a\tag{1}\\b\end{align}`, false)
	if len(tagged.Texts) != 1 || tagged.Texts[0].Text != "(1)" {
		t.Fatalf("row tag texts %+v", tagged.Texts)
	}
	near(t, "row tag baseline", tagged.Texts[0].Y, only(t, tagged, "a").Y)
	near(t, "row tag x", tagged.Texts[0].X, 500+tagGapEm*em)
	star := render(t, `x\tag*{A}`, false)
	if len(star.Texts) != 1 || star.Texts[0].Text != "A" {
		t.Fatalf("starred tag texts %+v", star.Texts)
	}
}

func TestTextAndMissingGlyphsUseTheDocumentFace(t *testing.T) {
	out := render(t, `\text{if }가+x`, false)
	var texts []string
	for _, run := range out.Texts {
		texts = append(texts, run.Text)
	}
	if strings.Join(texts, "|") != "if |가" {
		t.Fatalf("text runs %q", texts)
	}
	near(t, "text size", out.Texts[0].Size, em)
	scripted := render(t, `x^{\text{ab}}`, false)
	near(t, "script text size", scripted.Texts[0].Size, em*script)
}

func finite(out *Layout) bool {
	for _, value := range []float32{out.Width, out.Ascent, out.Descent} {
		if stdmath.IsNaN(float64(value)) || stdmath.IsInf(float64(value), 0) {
			return false
		}
	}
	return true
}

func TestEveryProductionLaysOutInBothStyles(t *testing.T) {
	for _, production := range productions {
		for _, display := range []bool{false, true} {
			if out := render(t, production.source, display); !finite(out) {
				t.Fatalf("Layout(%q) has non-finite extent %+v", production.source, out)
			}
		}
	}
}

func TestLayoutRefusesBeyondTheDrawingBudget(t *testing.T) {
	source := `\left(\begin{matrix}` + strings.Repeat(`1\\`, MaxRows-1) + `1\end{matrix}` + strings.Repeat(`\middle(`, 5000) + `\right)`
	start := time.Now()
	_, err := testFont(t).Layout(mustParse(t, source), Options{Size: em, Text: halfEmText{}})
	if !errors.Is(err, errLayoutBudget) {
		t.Fatalf("Layout = %v, want the drawing budget error", err)
	}
	if elapsed := time.Since(start); elapsed > 10*time.Second {
		t.Fatalf("budget refusal took %v", elapsed)
	}
}

func TestLayoutRejectsMissingInputs(t *testing.T) {
	formula := mustParse(t, `x`)
	if _, err := testFont(t).Layout(formula, Options{Size: em}); err == nil {
		t.Fatal("Layout without a text measurer succeeded")
	}
	if _, err := testFont(t).Layout(formula, Options{Size: float32(stdmath.NaN()), Text: halfEmText{}}); err == nil {
		t.Fatal("Layout with a NaN size succeeded")
	}
}

func FuzzLayout(f *testing.F) {
	for _, production := range productions {
		f.Add(production.source, false)
	}
	f.Add(`\left(\sqrt[\frac{1}{2}]{\begin{pmatrix}a\\b\end{pmatrix}}\middle|\overbrace{x}^{y}\right)`, true)
	font, loadErr := loadFixture()
	if loadErr != nil {
		f.Fatal(loadErr)
	}
	f.Fuzz(func(t *testing.T, source string, display bool) {
		formula, err := Parse(source)
		if err != nil {
			return
		}
		out, err := font.Layout(formula, Options{Size: 16, Display: display, Text: halfEmText{}})
		if errors.Is(err, errLayoutBudget) {
			return
		}
		if err != nil {
			t.Fatalf("Layout(%q): %v", source, err)
		}
		if len(out.Glyphs)+len(out.Rules)+len(out.Texts) > MaxLayoutItems {
			t.Fatalf("Layout(%q) emitted more than %d items", source, MaxLayoutItems)
		}
		if !finite(out) {
			t.Fatalf("Layout(%q) has non-finite extent %+v", source, out)
		}
	})
}
