package math

import (
	"bytes"
	"errors"
	"fmt"
	"sync"

	"github.com/go-text/typesetting/di"
	"github.com/go-text/typesetting/font"
	"github.com/go-text/typesetting/shaping"
	"golang.org/x/image/math/fixed"
)

type TextMetrics struct {
	Width, Ascent, Descent float32
}

type TextFace interface {
	MeasureText(text string, size float32) TextMetrics
	AppendOutline(path []Segment, text string, size float32) []Segment
}

type fallback []*font.Face

func (faces fallback) ResolveFace(r rune) *font.Face {
	for _, face := range faces {
		if _, ok := face.NominalGlyph(r); ok {
			return face
		}
	}
	return faces[0]
}

type TextFont struct {
	mu        sync.Mutex
	faces     fallback
	shaper    shaping.HarfbuzzShaper
	segmenter shaping.Segmenter
	runes     []rune
}

func NewTextFont(faces ...[]byte) (*TextFont, error) {
	if len(faces) == 0 {
		return nil, errors.New("math text needs a font face; pass the document text faces")
	}
	parsed := make(fallback, len(faces))
	for i, data := range faces {
		face, err := font.ParseTTF(bytes.NewReader(data))
		if err != nil {
			return nil, fmt.Errorf("read math text face %d: %w", i+1, err)
		}
		parsed[i] = face
	}
	return &TextFont{faces: parsed}, nil
}

func (t *TextFont) shape(text string, size float32, visit func(run shaping.Output, x float32)) {
	t.runes = t.runes[:0]
	for _, r := range text {
		t.runes = append(t.runes, r)
	}
	input := shaping.Input{Text: t.runes, RunEnd: len(t.runes), Direction: di.DirectionLTR, Size: fixed.Int26_6(size * 64)}
	var x float32
	for _, run := range t.segmenter.Split(input, t.faces) {
		output := t.shaper.Shape(run)
		visit(output, x)
		x += fixedFloat(output.Advance)
	}
}

func (t *TextFont) MeasureText(text string, size float32) TextMetrics {
	t.mu.Lock()
	defer t.mu.Unlock()
	var metrics TextMetrics
	t.shape(text, size, func(run shaping.Output, _ float32) {
		metrics.Width += fixedFloat(run.Advance)
		metrics.Ascent = max(metrics.Ascent, fixedFloat(run.LineBounds.Ascent))
		metrics.Descent = max(metrics.Descent, -fixedFloat(run.LineBounds.Descent))
	})
	return metrics
}

func (t *TextFont) AppendOutline(path []Segment, text string, size float32) []Segment {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.shape(text, size, func(run shaping.Output, x float32) {
		scale := fixedFloat(run.Size) / float32(run.Face.Upem())
		for _, glyph := range run.Glyphs {
			origin := Point{X: x + fixedFloat(glyph.XOffset), Y: -fixedFloat(glyph.YOffset)}
			path = appendGlyphOutline(path, run.Face, glyph.GlyphID, origin, scale)
			x += fixedFloat(glyph.Advance)
		}
	})
	return path
}

func fixedFloat(v fixed.Int26_6) float32 {
	return float32(v) / 64
}
