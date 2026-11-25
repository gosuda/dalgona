package math

import (
	"io"
	"strconv"

	"github.com/go-text/typesetting/font/opentype"
)

const svgCommands = "MLQC"

type svgWriter struct {
	w   io.Writer
	buf []byte
	err error
}

func (s *svgWriter) flush() {
	if s.err == nil {
		_, s.err = s.w.Write(s.buf)
	}
	s.buf = s.buf[:0]
}

func (s *svgWriter) text(t string) {
	s.buf = append(s.buf, t...)
}

func (s *svgWriter) number(v float32) {
	s.buf = strconv.AppendFloat(s.buf, float64(v), 'f', -1, 32)
}

func (s *svgWriter) pair(x, y float32) {
	s.number(x)
	s.buf = append(s.buf, ' ')
	s.number(y)
}

func (s *svgWriter) pathData(path []Segment) {
	for i, segment := range path {
		if segment.Op == opentype.SegmentOpMoveTo && i > 0 {
			s.text("Z ")
		}
		s.buf = append(s.buf, svgCommands[segment.Op])
		for _, p := range segment.points() {
			s.buf = append(s.buf, ' ')
			s.pair(p.X, p.Y)
		}
		s.buf = append(s.buf, ' ')
	}
	s.text("Z")
}

func (s *svgWriter) path(path []Segment) bool {
	s.text(`<path d="`)
	s.pathData(path)
	s.text("\"/>\n")
	s.flush()
	return s.err == nil
}

func (l *Layout) WriteSVG(w io.Writer) error {
	s := &svgWriter{w: w}
	height := l.Ascent + l.Descent
	s.text(`<?xml version="1.0" encoding="UTF-8"?>` + "\n")
	s.text(`<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="`)
	s.number(l.Width)
	s.text(`" height="`)
	s.number(height)
	s.text(`" viewBox="0 0 `)
	s.pair(l.Width, height)
	s.text("\">\n")
	s.flush()
	drawn := l.writeGlyphDefinitions(s)
	l.writeGlyphUses(s, drawn)
	l.outlineRulesAndText(nil, s.path)
	s.text("</svg>\n")
	s.flush()
	return s.err
}

func (l *Layout) writeGlyphDefinitions(s *svgWriter) map[uint32]bool {
	drawn := make(map[uint32]bool)
	var path []Segment
	s.text("<defs>\n")
	for _, glyph := range l.Glyphs {
		if _, seen := drawn[glyph.ID]; seen {
			continue
		}
		path = l.font.appendOutline(path[:0], Glyph{ID: glyph.ID, Size: l.font.upem}, Point{})
		drawn[glyph.ID] = len(path) > 0
		if len(path) == 0 {
			continue
		}
		s.text(`<path id="g`)
		s.buf = strconv.AppendUint(s.buf, uint64(glyph.ID), 10)
		s.text(`" d="`)
		s.pathData(path)
		s.text("\"/>\n")
		s.flush()
	}
	s.text("</defs>\n")
	return drawn
}

func (l *Layout) writeGlyphUses(s *svgWriter, drawn map[uint32]bool) {
	for _, glyph := range l.Glyphs {
		if !drawn[glyph.ID] {
			continue
		}
		scale := glyph.Size / l.font.upem
		s.text(`<use xlink:href="#g`)
		s.buf = strconv.AppendUint(s.buf, uint64(glyph.ID), 10)
		s.text(`" transform="matrix(`)
		s.pair(scale, 0)
		s.buf = append(s.buf, ' ')
		s.pair(0, scale)
		s.buf = append(s.buf, ' ')
		s.pair(glyph.X, l.Ascent+glyph.Y)
		s.text(")\"/>\n")
		s.flush()
	}
}
