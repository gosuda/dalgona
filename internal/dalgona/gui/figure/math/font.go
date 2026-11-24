package math

import (
	"bytes"
	"errors"
	"fmt"
	"sync"

	"github.com/go-text/typesetting/font"
	"github.com/go-text/typesetting/font/opentype"
)

type Font struct {
	mu              sync.Mutex
	face            *font.Face
	upem            float32
	table           *mathTable
	scriptStyle     []font.GSUBLookup
	flattenedAccent []font.GSUBLookup
}

func ParseFont(data []byte) (*Font, error) {
	loader, err := opentype.NewLoader(bytes.NewReader(data))
	if err != nil {
		return nil, fmt.Errorf("read math font: %w", err)
	}
	raw, err := loader.RawTable(opentype.MustNewTag("MATH"))
	if err != nil {
		return nil, fmt.Errorf("math font has no MATH table; bundle an OpenType math font such as STIX Two Math: %w", err)
	}
	table, err := parseMathTable(raw)
	if err != nil {
		return nil, err
	}
	parsed, err := font.NewFont(loader)
	if err != nil {
		return nil, fmt.Errorf("read math font tables: %w", err)
	}
	if parsed.Upem() == 0 {
		return nil, errors.New("math font declares zero units per em; use a valid OpenType math font")
	}
	return &Font{
		face:            font.NewFace(parsed),
		upem:            float32(parsed.Upem()),
		table:           table,
		scriptStyle:     featureLookups(&parsed.GSUB, scriptStyleFeature),
		flattenedAccent: featureLookups(&parsed.GSUB, flattenedAccentFeature),
	}, nil
}

type glyphMetrics struct {
	advance           float32
	ascent, descent   float32
	inkLeft, inkRight float32
}

func (f *Font) lookup(r rune) (uint32, bool) {
	glyph, ok := f.face.NominalGlyph(r)
	return uint32(glyph), ok && glyph != 0
}

func (f *Font) metrics(glyph uint32) glyphMetrics {
	extents, ok := f.face.GlyphExtents(font.GID(glyph))
	metrics := glyphMetrics{advance: f.face.HorizontalAdvance(font.GID(glyph))}
	if ok {
		metrics.ascent = extents.YBearing
		metrics.descent = -(extents.YBearing + extents.Height)
		metrics.inkLeft = extents.XBearing
		metrics.inkRight = extents.XBearing + extents.Width
	}
	return metrics
}
