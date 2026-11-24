package math

import (
	"slices"

	"github.com/go-text/typesetting/font"
	"github.com/go-text/typesetting/font/opentype"
	"github.com/go-text/typesetting/font/opentype/tables"
)

var (
	mathScript             = opentype.MustNewTag("math")
	defaultScript          = opentype.MustNewTag("DFLT")
	scriptStyleFeature     = opentype.MustNewTag("ssty")
	flattenedAccentFeature = opentype.MustNewTag("flac")
)

func featureLookups(gsub *font.GSUB, feature font.Tag) []font.GSUBLookup {
	for _, script := range [...]font.Tag{mathScript, defaultScript} {
		if indices, ok := scriptFeature(gsub, script, feature); ok {
			return selectLookups(gsub, indices)
		}
	}
	return nil
}

func scriptFeature(gsub *font.GSUB, script, feature font.Tag) ([]uint16, bool) {
	index := gsub.FindScript(script)
	if index < 0 || gsub.Scripts[index].DefaultLangSys == nil {
		return nil, false
	}
	for _, i := range gsub.Scripts[index].DefaultLangSys.FeatureIndices {
		if int(i) < len(gsub.Features) && gsub.Features[i].Tag == feature {
			return gsub.Features[i].LookupListIndices, true
		}
	}
	return nil, false
}

func selectLookups(gsub *font.GSUB, indices []uint16) []font.GSUBLookup {
	lookups := make([]font.GSUBLookup, 0, len(indices))
	for _, i := range slices.Sorted(slices.Values(indices)) {
		if int(i) < len(gsub.Lookups) {
			lookups = append(lookups, gsub.Lookups[i])
		}
	}
	return lookups
}

func substitute(lookups []font.GSUBLookup, glyph uint32, alternate int) uint32 {
	for _, lookup := range lookups {
		glyph = applyLookup(lookup, glyph, alternate)
	}
	return glyph
}

func applyLookup(lookup font.GSUBLookup, glyph uint32, alternate int) uint32 {
	if glyph > 0xFFFF {
		return glyph
	}
	for _, subtable := range lookup.Subtables {
		if index, ok := subtable.Cov().Index(tables.GlyphID(glyph)); ok {
			return substituted(subtable, glyph, index, alternate)
		}
	}
	return glyph
}

func substituted(subtable tables.GSUBLookup, glyph uint32, index, alternate int) uint32 {
	switch s := subtable.(type) {
	case tables.SingleSubs:
		return singleSubstitute(s.Data, glyph, index)
	case tables.AlternateSubs:
		return alternateSubstitute(s, glyph, index, alternate)
	}
	return glyph
}

func singleSubstitute(data tables.SingleSubstData, glyph uint32, index int) uint32 {
	switch d := data.(type) {
	case tables.SingleSubstData1:
		next := (int(glyph) + int(d.DeltaGlyphID)) & 0xFFFF
		return uint32(next)
	case tables.SingleSubstData2:
		if index < len(d.SubstituteGlyphIDs) {
			return uint32(d.SubstituteGlyphIDs[index])
		}
	}
	return glyph
}

func alternateSubstitute(s tables.AlternateSubs, glyph uint32, index, alternate int) uint32 {
	if index >= len(s.AlternateSets) || len(s.AlternateSets[index].AlternateGlyphIDs) == 0 {
		return glyph
	}
	alternates := s.AlternateSets[index].AlternateGlyphIDs
	return uint32(alternates[min(alternate, len(alternates)-1)])
}
