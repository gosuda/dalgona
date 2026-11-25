package math

import (
	stdmath "math"
	"strings"
	"testing"
)

func fixtureMathTable(t *testing.T) []byte {
	t.Helper()
	return fixtureMath(buildFixture().glyphs)
}

func TestMathConstantsFollowTheOpenTypeRecordOrder(t *testing.T) {
	table, err := parseMathTable(fixtureMathTable(t))
	if err != nil {
		t.Fatal(err)
	}
	if len(fixtureConstants) != int(constantCount) {
		t.Fatalf("fixture lists %d constants, the table declares %d", len(fixtureConstants), constantCount)
	}
	for _, record := range fixtureConstants {
		if got := table.constant(record.id).value; got != record.value {
			t.Errorf("%s = %d, want %d", record.name, got, record.value)
		}
	}
	scalars := map[string][2]int{
		"ScriptPercentScaleDown":          {table.scriptPercent, fixtureScriptPercent},
		"ScriptScriptPercentScaleDown":    {table.scriptScriptPercent, fixtureScriptScriptPercent},
		"DisplayOperatorMinHeight":        {table.displayOperatorMin, fixtureDisplayOperatorMin},
		"RadicalDegreeBottomRaisePercent": {table.degreeRaisePercent, fixtureDegreeRaisePercent},
		"MinConnectorOverlap":             {table.minConnectorOverlap, fixtureMinConnectorOverlap},
	}
	for name, pair := range scalars {
		if pair[0] != pair[1] {
			t.Errorf("%s = %d, want %d", name, pair[0], pair[1])
		}
	}
}

func fixtureTable(t *testing.T) *mathTable {
	t.Helper()
	table, err := parseMathTable(fixtureMathTable(t))
	if err != nil {
		t.Fatal(err)
	}
	return table
}

func TestMathGlyphValueLookups(t *testing.T) {
	table := fixtureTable(t)
	if got := table.italicCorrection(gid("∫")).value; got != 150 {
		t.Errorf("italic correction of integral = %d, want 150", got)
	}
	if got := table.italicCorrection(gid("x")); got != (mathValue{}) {
		t.Errorf("italic correction of uncovered glyph = %+v, want none", got)
	}
	if got, ok := table.topAccentAttachment(gid("hat")); !ok || got.value != -250 {
		t.Errorf("hat attachment = %+v %v, want -250", got, ok)
	}
}

func TestMathKernLookups(t *testing.T) {
	table := fixtureTable(t)
	units := pixelScale{scale: 1, ppem: 1000}
	for height, want := range map[float32]float32{-1000: -80, 399: -80, 400: -30, 5000: -30} {
		if got := table.kern(gid("f"), cornerTopRight, height, units); got != want {
			t.Errorf("kern at %v = %v, want %v", height, got, want)
		}
	}
	if got := table.kern(gid("f"), cornerBottomRight, 0, units); got != 0 {
		t.Errorf("kern of an absent corner = %v, want 0", got)
	}
}

func TestMathVerticalConstructionLookups(t *testing.T) {
	table := fixtureTable(t)
	vertical, found := table.verticalConstruction(gid("√"))
	if !found || vertical.variants != 2 || vertical.variant(1) != (glyphVariant{glyph: gid("radical.v1"), advance: 1800}) {
		t.Errorf("radical construction = %+v %v", vertical, found)
	}
	if _, uncovered := table.verticalConstruction(gid(")")); uncovered {
		t.Error("uncovered glyph has a vertical construction")
	}
}

func TestMathHorizontalAssemblyLookup(t *testing.T) {
	horizontal, found := fixtureTable(t).horizontalConstruction(gid("⏞"))
	parts := horizontal.parts()
	if !found || len(parts) != 5 || !parts[1].extender || parts[0].extender || parts[4].glyph != gid("brace.right") {
		t.Errorf("brace assembly = %+v %v", parts, found)
	}
}

func TestMathTableRejectsEveryTruncation(t *testing.T) {
	data := fixtureMathTable(t)
	for size := range len(data) {
		if _, err := parseMathTable(data[:size]); err == nil {
			t.Fatalf("parseMathTable accepted %d of %d bytes", size, len(data))
		}
	}
}

func TestMathTableRejectsUnsupportedHeaders(t *testing.T) {
	data := fixtureMathTable(t)
	version := append([]byte(nil), data...)
	version[1] = 2
	if _, err := parseMathTable(version); err == nil || !strings.Contains(err.Error(), "version") {
		t.Errorf("version 2 table: %v", err)
	}
	scale := append([]byte(nil), data...)
	scale[10], scale[11] = 0, 0
	if _, err := parseMathTable(scale); err == nil || !strings.Contains(err.Error(), "script scale") {
		t.Errorf("zero script scale: %v", err)
	}
	coverage := append([]byte(nil), data...)
	info := u16At(data, 6)
	italics := info + u16At(data, info)
	start := italics + u16At(data, italics)
	coverage[start], coverage[start+1] = 0, 3
	if _, err := parseMathTable(coverage); err == nil || !strings.Contains(err.Error(), "format 3") {
		t.Errorf("coverage format 3: %v", err)
	}
}

func u16At(data []byte, at int) int {
	return int(data[at])<<8 | int(data[at+1])
}

func TestParseFontRequiresAMathTable(t *testing.T) {
	fixture := buildFixture().data
	if _, err := ParseFont(fixture[:len(fixture)/2]); err == nil {
		t.Error("ParseFont accepted a truncated font")
	}
	if _, err := ParseFont([]byte("not a font")); err == nil {
		t.Error("ParseFont accepted arbitrary bytes")
	}
}

func FuzzMathTable(f *testing.F) {
	f.Add(fixtureMath(buildFixture().glyphs))
	f.Fuzz(func(t *testing.T, data []byte) {
		table, err := parseMathTable(data)
		if err != nil {
			return
		}
		for glyph := range uint32(128) {
			exerciseLookups(table, glyph)
		}
	})
}

func exerciseLookups(table *mathTable, glyph uint32) {
	table.italicCorrection(glyph)
	table.topAccentAttachment(glyph)
	table.extendedShape(glyph)
	for at := range corner(4) {
		table.kern(glyph, at, 300, pixelScale{scale: 0.02, ppem: 20})
	}
	for _, lookup := range []func(uint32) (construction, bool){table.verticalConstruction, table.horizontalConstruction} {
		c, ok := lookup(glyph)
		if !ok {
			continue
		}
		for i := range c.variants {
			c.variant(i)
		}
		planAssembly(c.parts(), 5000, float32(table.minConnectorOverlap), maxAssemblyParts)
		table.pixels(c.assemblyItalic(), pixelScale{scale: 0.02, ppem: 20})
	}
}

func TestAssemblyPlanMeetsTargetWithinConnectorOverlaps(t *testing.T) {
	parts := []assemblyPart{
		{glyph: 1, start: 0, end: 200, full: 600},
		{glyph: 2, start: 200, end: 200, full: 400, extender: true},
		{glyph: 3, start: 200, end: 0, full: 600},
	}
	const overlap = 50
	for target := float32(0); target < 20000; target += 137 {
		plan := planAssembly(parts, target, overlap, maxAssemblyParts)
		extenders := float32(len(plan.glyphs) - 2)
		longest := 1200 + 400*extenders - overlap*(extenders+1)
		capacity := (200 - overlap) * (extenders + 1)
		if want := max(target, longest-capacity); stdmath.Abs(float64(plan.size-want)) > 0.1 {
			t.Fatalf("target %v planned %v, want %v", target, plan.size, want)
		}
		if extenders > 0 && longest-(400-overlap) >= target {
			t.Fatalf("target %v used %v extenders; one fewer reaches it", target, extenders)
		}
		for i := 1; i < len(plan.offsets); i++ {
			used := plan.offsets[i-1] + float32(partFull(parts, plan.glyphs[i-1])) - plan.offsets[i]
			if used < overlap-0.1 || used > 200+0.1 {
				t.Fatalf("target %v joint %d overlaps %v", target, i, used)
			}
		}
	}
	capped := planAssembly(parts, 1e9, overlap, 10)
	if len(capped.glyphs) > 10 {
		t.Fatalf("assembly exceeded its part limit with %d glyphs", len(capped.glyphs))
	}
}

func partFull(parts []assemblyPart, glyph uint32) int {
	for _, part := range parts {
		if part.glyph == glyph {
			return part.full
		}
	}
	return 0
}
