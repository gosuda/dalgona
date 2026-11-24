package math

import (
	"errors"
	"fmt"
)

type constant uint8

const (
	mathLeading constant = iota
	axisHeight
	accentBaseHeight
	flattenedAccentBaseHeight
	subscriptShiftDown
	subscriptTopMax
	subscriptBaselineDropMin
	superscriptShiftUp
	superscriptShiftUpCramped
	superscriptBottomMin
	superscriptBaselineDropMax
	subSuperscriptGapMin
	superscriptBottomMaxWithSubscript
	spaceAfterScript
	upperLimitGapMin
	upperLimitBaselineRiseMin
	lowerLimitGapMin
	lowerLimitBaselineDropMin
	stackTopShiftUp
	stackTopDisplayStyleShiftUp
	stackBottomShiftDown
	stackBottomDisplayStyleShiftDown
	stackGapMin
	stackDisplayStyleGapMin
	stretchStackTopShiftUp
	stretchStackBottomShiftDown
	stretchStackGapAboveMin
	stretchStackGapBelowMin
	fractionNumeratorShiftUp
	fractionNumeratorDisplayStyleShiftUp
	fractionDenominatorShiftDown
	fractionDenominatorDisplayStyleShiftDown
	fractionNumeratorGapMin
	fractionNumDisplayStyleGapMin
	fractionRuleThickness
	fractionDenominatorGapMin
	fractionDenomDisplayStyleGapMin
	skewedFractionHorizontalGap
	skewedFractionVerticalGap
	overbarVerticalGap
	overbarRuleThickness
	overbarExtraAscender
	underbarVerticalGap
	underbarRuleThickness
	underbarExtraDescender
	radicalVerticalGap
	radicalDisplayStyleVerticalGap
	radicalRuleThickness
	radicalExtraAscender
	radicalKernBeforeDegree
	radicalKernAfterDegree
	constantCount
)

const (
	mathHeaderSize       = 10
	mathConstantsSize    = 8 + 4*int(constantCount) + 2
	mathValueRecordSize  = 4
	kernRecordSize       = 8
	variantRecordSize    = 4
	partRecordSize       = 10
	glyphInfoSize        = 8
	variantsHeaderSize   = 10
	constructionHeadSize = 4
	assemblyHeadSize     = 6
	deviceHeaderSize     = 6
	extenderFlag         = 1
)

type corner uint8

const (
	cornerTopRight corner = iota
	cornerTopLeft
	cornerBottomRight
	cornerBottomLeft
)

type tableReader struct {
	data []byte
	err  error
}

func (r *tableReader) u16(at int) int {
	if r.err != nil {
		return 0
	}
	if at < 0 || at+2 > len(r.data) {
		r.err = fmt.Errorf("MATH table offset %d lies outside its %d bytes", at, len(r.data))
		return 0
	}
	return int(r.data[at])<<8 | int(r.data[at+1])
}

func (r *tableReader) i16(at int) int {
	value := r.u16(at)
	if value >= 0x8000 {
		value -= 0x10000
	}
	return value
}

func (r *tableReader) glyph(at int) uint32 {
	r.u16(at)
	if r.err != nil {
		return 0
	}
	return uint32(r.data[at])<<8 | uint32(r.data[at+1])
}

func (r *tableReader) need(at, size int, what string) {
	if r.err == nil && (at < 0 || size < 0 || at+size > len(r.data)) {
		r.err = fmt.Errorf("MATH %s at offset %d needs %d bytes beyond the %d-byte table", what, at, size, len(r.data))
	}
}

func (r *tableReader) offset(base, field int) int {
	relative := r.u16(field)
	if relative == 0 {
		return 0
	}
	return base + relative
}

type mathValue struct {
	value, device int
}

type pixelScale struct {
	scale float32
	ppem  int
}

func readValue(r *tableReader, parent, at int) mathValue {
	return mathValue{value: r.i16(at), device: r.offset(parent, at+2)}
}

type valueTable struct {
	base, coverage, count, records int
}

type kernTable struct {
	base, coverage, count int
}

type constructionTable struct {
	base, coverage, count, offsets int
}

type mathTable struct {
	reader               tableReader
	scriptPercent        int
	scriptScriptPercent  int
	displayOperatorMin   int
	degreeRaisePercent   int
	values               [constantCount]mathValue
	italics, topAccents  valueTable
	extended             int
	kerns                kernTable
	minConnectorOverlap  int
	vertical, horizontal constructionTable
}

func parseMathTable(data []byte) (*mathTable, error) {
	t := &mathTable{reader: tableReader{data: data}}
	r := &t.reader
	r.need(0, mathHeaderSize, "header")
	if version := r.u16(0); r.err == nil && version != 1 {
		return nil, fmt.Errorf("MATH table major version %d is not supported; use a version 1 math font", version)
	}
	t.parseConstants(r.offset(0, 4))
	t.parseGlyphInfo(r.offset(0, 6))
	t.parseVariants(r.offset(0, 8))
	if r.err != nil {
		return nil, r.err
	}
	if err := t.validateScales(); err != nil {
		return nil, err
	}
	return t, nil
}

func (t *mathTable) validateScales() error {
	for _, percent := range [...]int{t.scriptPercent, t.scriptScriptPercent} {
		if percent < 1 || percent > 100 {
			return fmt.Errorf("MATH constants declare a script scale of %d%%; use a font with scales from 1%% to 100%%", percent)
		}
	}
	return nil
}

func (t *mathTable) parseConstants(base int) {
	r := &t.reader
	if base == 0 {
		r.err = errors.New("MATH table has no constants subtable; use a complete OpenType math font")
		return
	}
	r.need(base, mathConstantsSize, "constants")
	t.scriptPercent = r.i16(base)
	t.scriptScriptPercent = r.i16(base + 2)
	t.displayOperatorMin = r.u16(base + 6)
	for i := range t.values {
		t.values[i] = readValue(r, base, base+8+i*mathValueRecordSize)
	}
	t.degreeRaisePercent = r.i16(base + 8 + int(constantCount)*mathValueRecordSize)
}

func (t *mathTable) parseGlyphInfo(base int) {
	if base == 0 {
		return
	}
	r := &t.reader
	r.need(base, glyphInfoSize, "glyph info")
	t.italics = t.parseValueTable(r.offset(base, base), "italics correction")
	t.topAccents = t.parseValueTable(r.offset(base, base+2), "top accent attachment")
	t.extended = r.offset(base, base+4)
	t.validateCoverage(t.extended)
	t.parseKerns(r.offset(base, base+6))
}

func (t *mathTable) parseValueTable(base int, what string) valueTable {
	if base == 0 {
		return valueTable{}
	}
	r := &t.reader
	r.need(base, 4, what)
	table := valueTable{base: base, coverage: r.offset(base, base), count: r.u16(base + 2), records: base + 4}
	r.need(table.records, table.count*mathValueRecordSize, what+" records")
	t.validateCoverage(table.coverage)
	return table
}

func (t *mathTable) validateCoverage(at int) {
	if at == 0 {
		return
	}
	r := &t.reader
	r.need(at, 4, "coverage")
	switch format := r.u16(at); format {
	case 1:
		r.need(at+4, 2*r.u16(at+2), "coverage glyphs")
	case 2:
		r.need(at+4, 6*r.u16(at+2), "coverage ranges")
	default:
		if r.err == nil {
			r.err = fmt.Errorf("MATH coverage at offset %d has unsupported format %d", at, format)
		}
	}
}

func (t *mathTable) parseKerns(base int) {
	if base == 0 {
		return
	}
	r := &t.reader
	r.need(base, 4, "kern info")
	t.kerns = kernTable{base: base, coverage: r.offset(base, base), count: r.u16(base + 2)}
	t.validateCoverage(t.kerns.coverage)
	r.need(base+4, t.kerns.count*kernRecordSize, "kern records")
	for i := 0; i < t.kerns.count*4 && r.err == nil; i++ {
		t.validateKern(r.offset(base, base+4+2*i))
	}
}

func (t *mathTable) validateKern(at int) {
	if at == 0 {
		return
	}
	r := &t.reader
	r.need(at, 2, "kern")
	heights := r.u16(at)
	r.need(at+2, (2*heights+1)*mathValueRecordSize, "kern values")
}

func (t *mathTable) parseVariants(base int) {
	if base == 0 {
		return
	}
	r := &t.reader
	r.need(base, variantsHeaderSize, "variants")
	t.minConnectorOverlap = r.u16(base)
	vertical := r.u16(base + 6)
	t.vertical = constructionTable{base: base, coverage: r.offset(base, base+2), count: vertical, offsets: base + variantsHeaderSize}
	t.horizontal = constructionTable{base: base, coverage: r.offset(base, base+4), count: r.u16(base + 8), offsets: base + variantsHeaderSize + 2*vertical}
	for _, table := range [...]constructionTable{t.vertical, t.horizontal} {
		t.validateCoverage(table.coverage)
		r.need(table.offsets, 2*table.count, "construction offsets")
		for i := 0; i < table.count && r.err == nil; i++ {
			t.validateConstruction(r.offset(base, table.offsets+2*i))
		}
	}
}

func (t *mathTable) validateConstruction(at int) {
	r := &t.reader
	if at == 0 {
		r.err = errors.New("MATH glyph construction offset is null; use a complete OpenType math font")
		return
	}
	r.need(at, constructionHeadSize, "glyph construction")
	r.need(at+constructionHeadSize, r.u16(at+2)*variantRecordSize, "glyph variants")
	assembly := r.offset(at, at)
	if assembly == 0 {
		return
	}
	r.need(assembly, assemblyHeadSize, "glyph assembly")
	r.need(assembly+assemblyHeadSize, r.u16(assembly+4)*partRecordSize, "glyph parts")
}

func (t *mathTable) constant(c constant) mathValue {
	return t.values[c]
}

func (t *mathTable) pixels(v mathValue, at pixelScale) float32 {
	return float32(v.value)*at.scale + float32(t.delta(v.device, at.ppem))
}

func (t *mathTable) delta(device, ppem int) int {
	r := tableReader{data: t.reader.data}
	start, end, format := r.u16(device), r.u16(device+2), r.u16(device+4)
	if device == 0 || format < 1 || format > 3 || ppem < start || ppem > end {
		return 0
	}
	bits := 1 << format
	perWord := 16 / bits
	index := ppem - start
	word := r.u16(device + deviceHeaderSize + 2*(index/perWord))
	raw := word >> (16 - bits*(index%perWord+1)) & (1<<bits - 1)
	if r.err != nil {
		return 0
	}
	if raw >= 1<<(bits-1) {
		raw -= 1 << bits
	}
	return raw
}

func (t *mathTable) coverageIndex(at int, glyph uint32) (int, bool) {
	if at == 0 {
		return 0, false
	}
	r := tableReader{data: t.reader.data}
	count := r.u16(at + 2)
	if r.u16(at) == 1 {
		return searchGlyphs(&r, at+4, count, glyph)
	}
	return searchRanges(&r, at+4, count, glyph)
}

func searchGlyphs(r *tableReader, at, count int, glyph uint32) (int, bool) {
	low, high := 0, count
	for low < high {
		middle := (low + high) / 2
		candidate := r.glyph(at + 2*middle)
		switch {
		case candidate == glyph:
			return middle, r.err == nil
		case candidate < glyph:
			low = middle + 1
		default:
			high = middle
		}
	}
	return 0, false
}

func searchRanges(r *tableReader, at, count int, glyph uint32) (int, bool) {
	low, high := 0, count
	for low < high {
		middle := (low + high) / 2
		record := at + 6*middle
		start, end := r.glyph(record), r.glyph(record+2)
		switch {
		case glyph < start:
			high = middle
		case glyph > end:
			low = middle + 1
		default:
			return r.u16(record+4) + int(glyph-start), r.err == nil
		}
	}
	return 0, false
}

func (t *mathTable) value(table valueTable, glyph uint32) (mathValue, bool) {
	index, ok := t.coverageIndex(table.coverage, glyph)
	if !ok || index >= table.count {
		return mathValue{}, false
	}
	r := tableReader{data: t.reader.data}
	value := readValue(&r, table.base, table.records+index*mathValueRecordSize)
	return value, r.err == nil
}

func (t *mathTable) italicCorrection(glyph uint32) mathValue {
	value, _ := t.value(t.italics, glyph)
	return value
}

func (t *mathTable) topAccentAttachment(glyph uint32) (mathValue, bool) {
	return t.value(t.topAccents, glyph)
}

func (t *mathTable) extendedShape(glyph uint32) bool {
	_, ok := t.coverageIndex(t.extended, glyph)
	return ok
}

func (t *mathTable) kern(glyph uint32, at corner, height float32, scale pixelScale) float32 {
	index, ok := t.coverageIndex(t.kerns.coverage, glyph)
	if !ok || index >= t.kerns.count {
		return 0
	}
	r := tableReader{data: t.reader.data}
	kern := r.offset(t.kerns.base, t.kerns.base+4+index*kernRecordSize+2*int(at))
	if kern == 0 {
		return 0
	}
	heights := r.u16(kern)
	step := 0
	for step < heights && height >= t.pixels(readValue(&r, kern, kern+2+step*mathValueRecordSize), scale) {
		step++
	}
	value := t.pixels(readValue(&r, kern, kern+2+(heights+step)*mathValueRecordSize), scale)
	if r.err != nil {
		return 0
	}
	return value
}

type construction struct {
	table    *mathTable
	at       int
	variants int
	assembly int
}

type glyphVariant struct {
	glyph   uint32
	advance int
}

type assemblyPart struct {
	glyph      uint32
	start, end int
	full       int
	extender   bool
}

func (t *mathTable) verticalConstruction(glyph uint32) (construction, bool) {
	return t.constructionIn(t.vertical, glyph)
}

func (t *mathTable) horizontalConstruction(glyph uint32) (construction, bool) {
	return t.constructionIn(t.horizontal, glyph)
}

func (t *mathTable) constructionIn(table constructionTable, glyph uint32) (construction, bool) {
	index, ok := t.coverageIndex(table.coverage, glyph)
	if !ok || index >= table.count {
		return construction{}, false
	}
	r := tableReader{data: t.reader.data}
	at := r.offset(table.base, table.offsets+2*index)
	found := construction{table: t, at: at, variants: r.u16(at + 2), assembly: r.offset(at, at)}
	return found, r.err == nil && at != 0
}

func (c construction) variant(i int) glyphVariant {
	r := tableReader{data: c.table.reader.data}
	record := c.at + constructionHeadSize + i*variantRecordSize
	return glyphVariant{glyph: r.glyph(record), advance: r.u16(record + 2)}
}

func (c construction) parts() []assemblyPart {
	if c.assembly == 0 {
		return nil
	}
	r := tableReader{data: c.table.reader.data}
	count := r.u16(c.assembly + 4)
	parts := make([]assemblyPart, 0, count)
	for i := range count {
		record := c.assembly + assemblyHeadSize + i*partRecordSize
		parts = append(parts, assemblyPart{
			glyph:    r.glyph(record),
			start:    r.u16(record + 2),
			end:      r.u16(record + 4),
			full:     r.u16(record + 6),
			extender: r.u16(record+8)&extenderFlag != 0,
		})
	}
	if r.err != nil {
		return nil
	}
	return parts
}

func (c construction) assemblyItalic() mathValue {
	if c.assembly == 0 {
		return mathValue{}
	}
	r := tableReader{data: c.table.reader.data}
	return readValue(&r, c.assembly, c.assembly)
}
