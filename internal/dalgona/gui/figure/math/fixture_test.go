package math

import (
	"slices"
	"sync"
	"testing"

	"github.com/go-text/typesetting/font/opentype"
)

type fixtureGlyph struct {
	name    string
	r       rune
	advance int
	ink     [4]int
}

type constantRecord struct {
	name  string
	id    constant
	value int
}

var fixtureConstants = []constantRecord{
	{"MathLeading", mathLeading, 150},
	{"AxisHeight", axisHeight, 250},
	{"AccentBaseHeight", accentBaseHeight, 450},
	{"FlattenedAccentBaseHeight", flattenedAccentBaseHeight, 650},
	{"SubscriptShiftDown", subscriptShiftDown, 150},
	{"SubscriptTopMax", subscriptTopMax, 350},
	{"SubscriptBaselineDropMin", subscriptBaselineDropMin, 160},
	{"SuperscriptShiftUp", superscriptShiftUp, 360},
	{"SuperscriptShiftUpCramped", superscriptShiftUpCramped, 290},
	{"SuperscriptBottomMin", superscriptBottomMin, 250},
	{"SuperscriptBaselineDropMax", superscriptBaselineDropMax, 230},
	{"SubSuperscriptGapMin", subSuperscriptGapMin, 200},
	{"SuperscriptBottomMaxWithSubscript", superscriptBottomMaxWithSubscript, 380},
	{"SpaceAfterScript", spaceAfterScript, 40},
	{"UpperLimitGapMin", upperLimitGapMin, 110},
	{"UpperLimitBaselineRiseMin", upperLimitBaselineRiseMin, 300},
	{"LowerLimitGapMin", lowerLimitGapMin, 120},
	{"LowerLimitBaselineDropMin", lowerLimitBaselineDropMin, 600},
	{"StackTopShiftUp", stackTopShiftUp, 450},
	{"StackTopDisplayStyleShiftUp", stackTopDisplayStyleShiftUp, 700},
	{"StackBottomShiftDown", stackBottomShiftDown, 350},
	{"StackBottomDisplayStyleShiftDown", stackBottomDisplayStyleShiftDown, 690},
	{"StackGapMin", stackGapMin, 150},
	{"StackDisplayStyleGapMin", stackDisplayStyleGapMin, 350},
	{"StretchStackTopShiftUp", stretchStackTopShiftUp, 300},
	{"StretchStackBottomShiftDown", stretchStackBottomShiftDown, 600},
	{"StretchStackGapAboveMin", stretchStackGapAboveMin, 80},
	{"StretchStackGapBelowMin", stretchStackGapBelowMin, 90},
	{"FractionNumeratorShiftUp", fractionNumeratorShiftUp, 400},
	{"FractionNumeratorDisplayStyleShiftUp", fractionNumeratorDisplayStyleShiftUp, 650},
	{"FractionDenominatorShiftDown", fractionDenominatorShiftDown, 350},
	{"FractionDenominatorDisplayStyleShiftDown", fractionDenominatorDisplayStyleShiftDown, 680},
	{"FractionNumeratorGapMin", fractionNumeratorGapMin, 40},
	{"FractionNumDisplayStyleGapMin", fractionNumDisplayStyleGapMin, 120},
	{"FractionRuleThickness", fractionRuleThickness, 60},
	{"FractionDenominatorGapMin", fractionDenominatorGapMin, 50},
	{"FractionDenomDisplayStyleGapMin", fractionDenomDisplayStyleGapMin, 130},
	{"SkewedFractionHorizontalGap", skewedFractionHorizontalGap, 350},
	{"SkewedFractionVerticalGap", skewedFractionVerticalGap, 70},
	{"OverbarVerticalGap", overbarVerticalGap, 140},
	{"OverbarRuleThickness", overbarRuleThickness, 55},
	{"OverbarExtraAscender", overbarExtraAscender, 45},
	{"UnderbarVerticalGap", underbarVerticalGap, 145},
	{"UnderbarRuleThickness", underbarRuleThickness, 57},
	{"UnderbarExtraDescender", underbarExtraDescender, 47},
	{"RadicalVerticalGap", radicalVerticalGap, 70},
	{"RadicalDisplayStyleVerticalGap", radicalDisplayStyleVerticalGap, 170},
	{"RadicalRuleThickness", radicalRuleThickness, 65},
	{"RadicalExtraAscender", radicalExtraAscender, 75},
	{"RadicalKernBeforeDegree", radicalKernBeforeDegree, 280},
	{"RadicalKernAfterDegree", radicalKernAfterDegree, -550},
}

const (
	fixtureScriptPercent       = 70
	fixtureScriptScriptPercent = 50
	fixtureDisplayOperatorMin  = 1500
	fixtureDegreeRaisePercent  = 60
	fixtureMinConnectorOverlap = 50
)

func fixtureConstant(id constant) float32 {
	for _, record := range fixtureConstants {
		if record.id == id {
			return float32(record.value)
		}
	}
	panic("fixture has no constant")
}

func fixtureGlyphs() []fixtureGlyph {
	glyphs := []fixtureGlyph{{name: ".notdef", advance: 500}}
	for r := 'a'; r <= 'z'; r++ {
		ink := [4]int{50, 0, 450, 450}
		switch {
		case slices.Contains([]rune("bdfhklt"), r):
			ink[3] = 700
		case slices.Contains([]rune("gjpqy"), r):
			ink[1] = -200
		}
		glyphs = append(glyphs, fixtureGlyph{string(r), r, 500, ink})
	}
	for r := 'A'; r <= 'Z'; r++ {
		glyphs = append(glyphs, fixtureGlyph{string(r), r, 700, [4]int{50, 0, 650, 700}})
	}
	for r := '0'; r <= '9'; r++ {
		glyphs = append(glyphs, fixtureGlyph{string(r), r, 500, [4]int{50, 0, 450, 650}})
	}
	return append(glyphs,
		fixtureGlyph{"+", '+', 800, [4]int{100, 0, 700, 500}},
		fixtureGlyph{"−", '−', 800, [4]int{100, 230, 700, 270}},
		fixtureGlyph{"=", '=', 800, [4]int{100, 150, 700, 350}},
		fixtureGlyph{",", ',', 300, [4]int{50, -150, 200, 100}},
		fixtureGlyph{"′", '′', 300, [4]int{50, 350, 250, 700}},
		fixtureGlyph{"(", '(', 400, [4]int{50, -250, 350, 750}},
		fixtureGlyph{")", ')', 400, [4]int{50, -250, 350, 750}},
		fixtureGlyph{"paren.v1", 0, 400, [4]int{50, -500, 350, 1000}},
		fixtureGlyph{"paren.bottom", 0, 400, [4]int{50, -100, 350, 500}},
		fixtureGlyph{"paren.extender", 0, 400, [4]int{50, 0, 350, 400}},
		fixtureGlyph{"paren.top", 0, 400, [4]int{50, 0, 350, 600}},
		fixtureGlyph{"√", '√', 800, [4]int{50, -300, 750, 900}},
		fixtureGlyph{"radical.v1", 0, 800, [4]int{50, -600, 750, 1200}},
		fixtureGlyph{"radical.bottom", 0, 800, [4]int{50, 0, 750, 1000}},
		fixtureGlyph{"radical.extender", 0, 800, [4]int{650, 0, 750, 500}},
		fixtureGlyph{"radical.top", 0, 800, [4]int{650, 0, 750, 600}},
		fixtureGlyph{"∑", '∑', 1000, [4]int{50, -250, 950, 750}},
		fixtureGlyph{"sum.display", 0, 1400, [4]int{50, -500, 1350, 1100}},
		fixtureGlyph{"∫", '∫', 500, [4]int{0, -300, 600, 800}},
		fixtureGlyph{"⏞", '⏞', 1000, [4]int{0, 500, 1000, 700}},
		fixtureGlyph{"brace.wide", 0, 1500, [4]int{0, 500, 1500, 700}},
		fixtureGlyph{"brace.left", 0, 400, [4]int{0, 500, 400, 700}},
		fixtureGlyph{"brace.extender", 0, 300, [4]int{0, 500, 300, 700}},
		fixtureGlyph{"brace.middle", 0, 400, [4]int{0, 500, 400, 700}},
		fixtureGlyph{"brace.right", 0, 400, [4]int{0, 500, 400, 700}},
		fixtureGlyph{"⏟", '⏟', 1000, [4]int{0, -200, 1000, 0}},
		fixtureGlyph{"hat", '\u0302', 0, [4]int{-400, 500, -100, 700}},
		fixtureGlyph{"hat.wide", 0, 1000, [4]int{0, 500, 1000, 700}},
		fixtureGlyph{"hat.flat", 0, 0, [4]int{-400, 500, -100, 620}},
		fixtureGlyph{"z.ssty1", 0, 520, [4]int{50, 0, 450, 420}},
		fixtureGlyph{"z.ssty2", 0, 540, [4]int{50, 0, 450, 400}},
		fixtureGlyph{"z.decoy", 0, 500, [4]int{50, 0, 450, 450}},
		fixtureGlyph{"k.ssty1", 0, 500, [4]int{50, 0, 450, 680}},
		fixtureGlyph{"q.alt", 0, 500, [4]int{50, -200, 450, 450}},
		fixtureGlyph{"q.ssty", 0, 500, [4]int{50, -200, 450, 450}},
	)
}

type fixtureFont struct {
	data   []byte
	glyphs map[string]uint32
}

var buildFixture = sync.OnceValue(func() fixtureFont {
	glyphs := fixtureGlyphs()
	ids := make(map[string]uint32, len(glyphs))
	var next uint32
	for _, glyph := range glyphs {
		ids[glyph.name] = next
		next++
	}
	tables := []opentype.Table{
		{Tag: opentype.MustNewTag("GSUB"), Content: fixtureGSUB(ids)},
		{Tag: opentype.MustNewTag("MATH"), Content: fixtureMath(ids)},
		{Tag: opentype.MustNewTag("cmap"), Content: fixtureCmap(glyphs)},
		{Tag: opentype.MustNewTag("glyf"), Content: fixtureGlyf(glyphs)},
		{Tag: opentype.MustNewTag("head"), Content: fixtureHead()},
		{Tag: opentype.MustNewTag("hhea"), Content: fixtureHhea(len(glyphs))},
		{Tag: opentype.MustNewTag("hmtx"), Content: fixtureHmtx(glyphs)},
		{Tag: opentype.MustNewTag("loca"), Content: fixtureLoca(glyphs)},
		{Tag: opentype.MustNewTag("maxp"), Content: fixtureMaxp(len(glyphs))},
	}
	return fixtureFont{data: opentype.WriteTTF(tables), glyphs: ids}
})

var loadFixture = sync.OnceValues(func() (*Font, error) {
	return ParseFont(buildFixture().data)
})

func testFont(t *testing.T) *Font {
	t.Helper()
	font, err := loadFixture()
	if err != nil {
		t.Fatalf("ParseFont(fixture): %v", err)
	}
	return font
}

func gid(name string) uint32 {
	id, ok := buildFixture().glyphs[name]
	if !ok {
		panic("fixture has no glyph " + name)
	}
	return id
}

type writer []byte

func (w *writer) u16(v int) { *w = append(*w, byte(v>>8&0xff), byte(v&0xff)) }
func (w *writer) u32(v int) {
	*w = append(*w, byte(v>>24&0xff), byte(v>>16&0xff), byte(v>>8&0xff), byte(v&0xff))
}

type recordValue struct {
	value  int
	device []byte
}

func (w *writer) record(v recordValue, deviceAt int, tail *writer) {
	w.u16(v.value)
	if v.device == nil {
		w.u16(0)
		return
	}
	w.u16(deviceAt + len(*tail))
	*tail = append(*tail, v.device...)
}

func deviceTable(start, end, format int, words ...int) []byte {
	var w writer
	w.u16(start)
	w.u16(end)
	w.u16(format)
	for _, word := range words {
		w.u16(word)
	}
	return w
}

func fixtureHead() []byte {
	var w writer
	w.u16(1)
	w.u16(0)
	w.u32(0x10000)
	w.u32(0)
	w.u32(0x5F0F3CF5)
	w.u16(0)
	w.u16(1000)
	w = append(w, make([]byte, 16)...)
	for _, v := range []int{-1000, -1000, 2000, 2000, 0, 8, 2, 1, 0} {
		w.u16(v)
	}
	return w
}

func fixtureHhea(count int) []byte {
	var w writer
	w.u32(0x10000)
	for _, v := range []int{800, -200, 0, 1500, 0, 0, 1500, 1, 0, 0, 0, 0, 0, 0, 0, count} {
		w.u16(v)
	}
	return w
}

func fixtureHmtx(glyphs []fixtureGlyph) []byte {
	var w writer
	for _, glyph := range glyphs {
		w.u16(glyph.advance)
		w.u16(glyph.ink[0])
	}
	return w
}

func fixtureMaxp(count int) []byte {
	var w writer
	w.u32(0x5000)
	w.u16(count)
	return w
}

func glyphOutline(glyph fixtureGlyph) []byte {
	if glyph.ink == [4]int{} {
		return nil
	}
	var w writer
	x0, y0, x1, y1 := glyph.ink[0], glyph.ink[1], glyph.ink[2], glyph.ink[3]
	for _, v := range []int{1, x0, y0, x1, y1, 3, 0} {
		w.u16(v)
	}
	w = append(w, 1, 1, 1, 1)
	for _, v := range []int{x0, 0, x1 - x0, 0, y0, y1 - y0, 0, y0 - y1} {
		w.u16(v)
	}
	return w
}

func fixtureGlyf(glyphs []fixtureGlyph) []byte {
	var w writer
	for _, glyph := range glyphs {
		w = append(w, glyphOutline(glyph)...)
	}
	return w
}

func fixtureLoca(glyphs []fixtureGlyph) []byte {
	var w writer
	offset := 0
	w.u32(offset)
	for _, glyph := range glyphs {
		offset += len(glyphOutline(glyph))
		w.u32(offset)
	}
	return w
}

func fixtureCmap(glyphs []fixtureGlyph) []byte {
	type mapping struct {
		r     rune
		glyph int
	}
	var mappings []mapping
	for i, glyph := range glyphs {
		if glyph.r != 0 {
			mappings = append(mappings, mapping{glyph.r, i})
		}
	}
	slices.SortFunc(mappings, func(a, b mapping) int { return int(a.r - b.r) })
	var w writer
	w.u16(0)
	w.u16(1)
	w.u16(3)
	w.u16(10)
	w.u32(12)
	w.u16(12)
	w.u16(0)
	w.u32(16 + 12*len(mappings))
	w.u32(0)
	w.u32(len(mappings))
	for _, m := range mappings {
		w.u32(int(m.r))
		w.u32(int(m.r))
		w.u32(m.glyph)
	}
	return w
}

func coverage1(glyphs ...uint32) []byte {
	slices.Sort(glyphs)
	var w writer
	w.u16(1)
	w.u16(len(glyphs))
	for _, glyph := range glyphs {
		w.u16(int(glyph))
	}
	return w
}

func coverage2(glyphs ...uint32) []byte {
	slices.Sort(glyphs)
	var w writer
	w.u16(2)
	w.u16(len(glyphs))
	for i, glyph := range glyphs {
		w.u16(int(glyph))
		w.u16(int(glyph))
		w.u16(i)
	}
	return w
}

type glyphValue struct {
	glyph uint32
	recordValue
}

func valueSubtable(entries []glyphValue) []byte {
	slices.SortFunc(entries, func(a, b glyphValue) int { return int(a.glyph) - int(b.glyph) })
	glyphs := make([]uint32, len(entries))
	for i, entry := range entries {
		glyphs[i] = entry.glyph
	}
	coverageAt := 4 + 4*len(entries)
	coverage := coverage1(glyphs...)
	var w, tail writer
	w.u16(coverageAt)
	w.u16(len(entries))
	for _, entry := range entries {
		w.record(entry.recordValue, coverageAt+len(coverage), &tail)
	}
	w = append(w, coverage...)
	return append(w, tail...)
}

func kernSubtable(glyph uint32, heights []int, kerns []recordValue) []byte {
	var kern, tail writer
	kernSize := 2 + 4*(len(heights)+len(kerns))
	kern.u16(len(heights))
	for _, v := range heights {
		kern.record(recordValue{value: v}, kernSize, &tail)
	}
	for _, v := range kerns {
		kern.record(v, kernSize, &tail)
	}
	kern = append(kern, tail...)
	var w writer
	recordEnd := 4 + 8
	w.u16(recordEnd + len(kern))
	w.u16(1)
	w.u16(recordEnd)
	w.u16(0)
	w.u16(0)
	w.u16(0)
	w = append(w, kern...)
	return append(w, coverage1(glyph)...)
}

func fixtureGlyphInfo(ids map[string]uint32) []byte {
	italics := valueSubtable([]glyphValue{
		{ids["∫"], recordValue{150, deviceTable(20, 20, 3, 0xFF00)}},
		{ids["f"], recordValue{value: 100}},
	})
	accents := valueSubtable([]glyphValue{
		{ids["x"], recordValue{value: 300}},
		{ids["hat"], recordValue{value: -250}},
		{ids["hat.wide"], recordValue{value: 500}},
		{ids["hat.flat"], recordValue{value: -250}},
	})
	extended := coverage1(ids["("])
	kerns := kernSubtable(ids["f"], []int{400}, []recordValue{{value: -80}, {-30, deviceTable(20, 20, 3, 0xFE00)}})
	var w writer
	w.u16(8)
	w.u16(8 + len(italics))
	w.u16(8 + len(italics) + len(accents))
	w.u16(8 + len(italics) + len(accents) + len(extended))
	w = append(w, italics...)
	w = append(w, accents...)
	w = append(w, extended...)
	return append(w, kerns...)
}

type part struct {
	glyph            string
	start, end, full int
	extender         bool
}

type variant struct {
	glyph   string
	advance int
}

type glyphConstruction struct {
	glyph    string
	variants []variant
	parts    []part
}

func constructionSubtable(ids map[string]uint32, c glyphConstruction) []byte {
	var w writer
	assemblyAt := 0
	if len(c.parts) > 0 {
		assemblyAt = 4 + 4*len(c.variants)
	}
	w.u16(assemblyAt)
	w.u16(len(c.variants))
	for _, v := range c.variants {
		w.u16(int(ids[v.glyph]))
		w.u16(v.advance)
	}
	if len(c.parts) == 0 {
		return w
	}
	w.u16(0)
	w.u16(0)
	w.u16(len(c.parts))
	for _, p := range c.parts {
		w.u16(int(ids[p.glyph]))
		w.u16(p.start)
		w.u16(p.end)
		w.u16(p.full)
		flags := 0
		if p.extender {
			flags = 1
		}
		w.u16(flags)
	}
	return w
}

var verticalConstructions = []glyphConstruction{
	{"(", []variant{{"(", 1000}, {"paren.v1", 1500}}, []part{
		{"paren.bottom", 0, 200, 600, false}, {"paren.extender", 200, 200, 400, true}, {"paren.top", 200, 0, 600, false},
	}},
	{"√", []variant{{"√", 1200}, {"radical.v1", 1800}}, []part{
		{"radical.bottom", 0, 300, 1000, false}, {"radical.extender", 300, 300, 500, true}, {"radical.top", 300, 0, 600, false},
	}},
	{"∑", []variant{{"∑", 1000}, {"sum.display", 1600}}, nil},
}

var horizontalConstructions = []glyphConstruction{
	{"hat", []variant{{"hat", 300}, {"hat.wide", 1000}}, nil},
	{"⏞", []variant{{"⏞", 1000}, {"brace.wide", 1500}}, []part{
		{"brace.left", 0, 100, 400, false},
		{"brace.extender", 100, 100, 300, true},
		{"brace.middle", 100, 100, 400, false},
		{"brace.extender", 100, 100, 300, true},
		{"brace.right", 100, 0, 400, false},
	}},
}

func sortedConstructions(ids map[string]uint32, constructions []glyphConstruction) []glyphConstruction {
	sorted := slices.Clone(constructions)
	slices.SortFunc(sorted, func(a, b glyphConstruction) int { return int(ids[a.glyph]) - int(ids[b.glyph]) })
	return sorted
}

func fixtureVariants(ids map[string]uint32) []byte {
	vertical := sortedConstructions(ids, verticalConstructions)
	horizontal := sortedConstructions(ids, horizontalConstructions)
	var body writer
	offsets := make([]int, 0, len(vertical)+len(horizontal))
	headerSize := 10 + 2*(len(vertical)+len(horizontal))
	var verticalGlyphs, horizontalGlyphs []uint32
	for _, c := range append(slices.Clone(vertical), horizontal...) {
		offsets = append(offsets, headerSize+len(body))
		body = append(body, constructionSubtable(ids, c)...)
	}
	for _, c := range vertical {
		verticalGlyphs = append(verticalGlyphs, ids[c.glyph])
	}
	for _, c := range horizontal {
		horizontalGlyphs = append(horizontalGlyphs, ids[c.glyph])
	}
	verticalCoverage := headerSize + len(body)
	body = append(body, coverage2(verticalGlyphs...)...)
	horizontalCoverage := headerSize + len(body)
	body = append(body, coverage1(horizontalGlyphs...)...)
	var w writer
	w.u16(fixtureMinConnectorOverlap)
	w.u16(verticalCoverage)
	w.u16(horizontalCoverage)
	w.u16(len(vertical))
	w.u16(len(horizontal))
	for _, offset := range offsets {
		w.u16(offset)
	}
	return append(w, body...)
}

var fixtureConstantDevices = map[constant][]byte{
	axisHeight:            deviceTable(20, 21, 2, 0x3E00),
	fractionRuleThickness: deviceTable(20, 20, 1, 0x4000),
}

func fixtureMathConstants() []byte {
	var w, tail writer
	w.u16(fixtureScriptPercent)
	w.u16(fixtureScriptScriptPercent)
	w.u16(1300)
	w.u16(fixtureDisplayOperatorMin)
	fixedSize := 8 + 4*len(fixtureConstants) + 2
	for _, record := range fixtureConstants {
		w.record(recordValue{record.value, fixtureConstantDevices[record.id]}, fixedSize, &tail)
	}
	w.u16(fixtureDegreeRaisePercent)
	return append(w, tail...)
}

func fixtureMath(ids map[string]uint32) []byte {
	constants := fixtureMathConstants()
	info := fixtureGlyphInfo(ids)
	variants := fixtureVariants(ids)
	var w writer
	w.u16(1)
	w.u16(0)
	w.u16(10)
	w.u16(10 + len(constants))
	w.u16(10 + len(constants) + len(info))
	w = append(w, constants...)
	w = append(w, info...)
	return append(w, variants...)
}
