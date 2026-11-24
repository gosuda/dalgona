package math

import (
	"slices"
	"testing"
)

func (w *writer) tag(s string) { *w = append(*w, s...) }

func singleDeltaSubtable(ids map[string]uint32, from, to string) []byte {
	var w writer
	w.u16(1)
	w.u16(6)
	w.u16(int(ids[to]) - int(ids[from]))
	return append(w, coverage1(ids[from])...)
}

func singleListSubtable(ids map[string]uint32, from, to string) []byte {
	var w writer
	w.u16(2)
	w.u16(8)
	w.u16(1)
	w.u16(int(ids[to]))
	return append(w, coverage1(ids[from])...)
}

type alternateSet struct {
	glyph      string
	alternates []string
}

func alternateSubtable(ids map[string]uint32, sets []alternateSet) []byte {
	slices.SortFunc(sets, func(a, b alternateSet) int { return int(ids[a.glyph]) - int(ids[b.glyph]) })
	var body writer
	offsets := make([]int, len(sets))
	glyphs := make([]uint32, len(sets))
	headerSize := 6 + 2*len(sets)
	for i, set := range sets {
		offsets[i] = headerSize + len(body)
		glyphs[i] = ids[set.glyph]
		body.u16(len(set.alternates))
		for _, alternate := range set.alternates {
			body.u16(int(ids[alternate]))
		}
	}
	var w writer
	w.u16(1)
	w.u16(headerSize + len(body))
	w.u16(len(sets))
	for _, offset := range offsets {
		w.u16(offset)
	}
	w = append(w, body...)
	return append(w, coverage1(glyphs...)...)
}

type taggedIndices struct {
	tag     string
	indices []int
}

func indexList(indices []int) []byte {
	var w writer
	w.u16(len(indices))
	for _, index := range indices {
		w.u16(index)
	}
	return w
}

func taggedList(records []taggedIndices, table func(indices []int) []byte) []byte {
	var body writer
	headerSize := 2 + 6*len(records)
	var w writer
	w.u16(len(records))
	for _, record := range records {
		w.tag(record.tag)
		w.u16(headerSize + len(body))
		body = append(body, table(record.indices)...)
	}
	return append(w, body...)
}

func scriptTable(features []int) []byte {
	var w writer
	w.u16(4)
	w.u16(0)
	w.u16(0)
	w.u16(0xFFFF)
	return append(w, indexList(features)...)
}

func featureTable(lookups []int) []byte {
	var w writer
	w.u16(0)
	return append(w, indexList(lookups)...)
}

type gsubLookup struct {
	kind     int
	subtable []byte
}

func lookupList(lookups []gsubLookup) []byte {
	var body writer
	headerSize := 2 + 2*len(lookups)
	var w writer
	w.u16(len(lookups))
	for _, lookup := range lookups {
		w.u16(headerSize + len(body))
		body.u16(lookup.kind)
		body.u16(0)
		body.u16(1)
		body.u16(8)
		body = append(body, lookup.subtable...)
	}
	return append(w, body...)
}

func fixtureGSUB(ids map[string]uint32) []byte {
	scripts := taggedList([]taggedIndices{{"DFLT", []int{2}}, {"math", []int{0, 1}}}, scriptTable)
	features := taggedList([]taggedIndices{{"flac", []int{2}}, {"ssty", []int{1, 0}}, {"ssty", []int{3}}}, featureTable)
	lookups := lookupList([]gsubLookup{
		{3, alternateSubtable(ids, []alternateSet{{"z", []string{"z.ssty1", "z.ssty2"}}, {"k", []string{"k.ssty1"}}, {"q", []string{"q.alt"}}})},
		{1, singleDeltaSubtable(ids, "q.alt", "q.ssty")},
		{1, singleListSubtable(ids, "hat", "hat.flat")},
		{1, singleListSubtable(ids, "z", "z.decoy")},
	})
	var w writer
	w.u16(1)
	w.u16(0)
	w.u16(10)
	w.u16(10 + len(scripts))
	w.u16(10 + len(scripts) + len(features))
	w = append(w, scripts...)
	w = append(w, features...)
	return append(w, lookups...)
}

func glyphAt(t *testing.T, out *Layout, x float32) Glyph {
	t.Helper()
	for _, glyph := range out.Glyphs {
		if glyph.X > x-1e-2 && glyph.X < x+1e-2 {
			return glyph
		}
	}
	t.Fatalf("layout has no glyph at x=%v: %+v", x, out.Glyphs)
	return Glyph{}
}

func TestScriptLevelsUseScriptStyleAlternates(t *testing.T) {
	out := render(t, `z^{z^{z}}`, false)
	only(t, out, "z")
	near(t, "script alternate size", only(t, out, "z.ssty1").Size, em*script)
	near(t, "scriptscript alternate size", only(t, out, "z.ssty2").Size, em*fixtureScriptScriptPercent/100)
	if len(glyphsNamed(out, "z.decoy")) != 0 {
		t.Fatal("a DFLT feature replaced the math script feature")
	}
	clamped := render(t, `k^{k^{k}}`, false)
	if len(glyphsNamed(clamped, "k.ssty1")) != 2 {
		t.Fatalf("scriptscript without a second alternate did not reuse the first: %+v", clamped.Glyphs)
	}
	chained := render(t, `q_{q}`, false)
	only(t, chained, "q.ssty")
	if len(glyphsNamed(chained, "q.alt")) != 0 {
		t.Fatal("script style lookups ran out of lookup list order")
	}
}

func TestScriptStyleAlternatesCarryTheirOwnMetrics(t *testing.T) {
	out := render(t, `a^{za}`, false)
	alternate := only(t, out, "z.ssty1")
	follower := glyphAt(t, out, alternate.X+520*script)
	if follower.ID != gid("a") {
		t.Fatalf("glyph after the script alternate = %+v, want a after its 520-unit advance", follower)
	}
}

func TestTallAccentBasesUseFlattenedAccents(t *testing.T) {
	low := render(t, `\hat{x}`, false)
	only(t, low, "hat")
	tall := render(t, `\hat{b}`, false)
	if len(glyphsNamed(tall, "hat")) != 0 {
		t.Fatalf("tall base kept the full accent: %+v", tall.Glyphs)
	}
	flat := only(t, tall, "hat.flat")
	base := only(t, tall, "b")
	near(t, "flattened accent attachment", flat.X, base.X+250-(-250))
	near(t, "flattened accent raise", -flat.Y, 700-c(accentBaseHeight))
}
