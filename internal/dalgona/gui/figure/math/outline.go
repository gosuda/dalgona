package math

import (
	"github.com/go-text/typesetting/font"
	"github.com/go-text/typesetting/font/opentype"
)

type Point struct {
	X, Y float32
}

type Segment struct {
	Op     opentype.SegmentOp
	Points [3]Point
}

func (s Segment) points() []Point {
	switch s.Op {
	case opentype.SegmentOpMoveTo, opentype.SegmentOpLineTo:
		return s.Points[:1]
	case opentype.SegmentOpQuadTo:
		return s.Points[:2]
	case opentype.SegmentOpCubeTo:
		return s.Points[:3]
	}
	return nil
}

func appendGlyphOutline(path []Segment, face *font.Face, glyph font.GID, origin Point, scale float32) []Segment {
	data, ok := face.GlyphData(glyph).(font.GlyphOutline)
	if !ok {
		return path
	}
	for _, segment := range data.Segments {
		out := Segment{Op: segment.Op}
		for j, arg := range segment.ArgsSlice() {
			out.Points[j] = Point{X: origin.X + arg.X*scale, Y: origin.Y - arg.Y*scale}
		}
		path = append(path, out)
	}
	return path
}

func (f *Font) appendOutline(path []Segment, glyph Glyph, origin Point) []Segment {
	f.mu.Lock()
	defer f.mu.Unlock()
	return appendGlyphOutline(path, f.face, font.GID(glyph.ID), Point{X: origin.X + glyph.X, Y: origin.Y + glyph.Y}, glyph.Size/f.upem)
}

func appendRectangle(path []Segment, left, top, right, bottom float32) []Segment {
	return append(path,
		Segment{Op: opentype.SegmentOpMoveTo, Points: [3]Point{{left, top}}},
		Segment{Op: opentype.SegmentOpLineTo, Points: [3]Point{{right, top}}},
		Segment{Op: opentype.SegmentOpLineTo, Points: [3]Point{{right, bottom}}},
		Segment{Op: opentype.SegmentOpLineTo, Points: [3]Point{{left, bottom}}},
	)
}

func (r Rule) appendOutline(path []Segment, origin Point) []Segment {
	left, top := origin.X+r.X, origin.Y+r.Y
	return appendRectangle(path, left, top, left+r.Width, top+r.Height)
}

func translate(path []Segment, by Point) {
	for i := range path {
		for j := range path[i].Points {
			path[i].Points[j].X += by.X
			path[i].Points[j].Y += by.Y
		}
	}
}

func (l *Layout) Paths(visit func(path []Segment) error) error {
	var err error
	l.outline(func(path []Segment) bool {
		err = visit(path)
		return err == nil
	})
	return err
}

func (l *Layout) outline(visit func(path []Segment) bool) {
	origin := Point{Y: l.Ascent}
	var path []Segment
	for _, glyph := range l.Glyphs {
		path = l.font.appendOutline(path[:0], glyph, origin)
		if len(path) > 0 && !visit(path) {
			return
		}
	}
	l.outlineRulesAndText(path, visit)
}

func (l *Layout) outlineRulesAndText(path []Segment, visit func(path []Segment) bool) {
	origin := Point{Y: l.Ascent}
	for _, rule := range l.Rules {
		path = rule.appendOutline(path[:0], origin)
		if !visit(path) {
			return
		}
	}
	for _, run := range l.Texts {
		path = l.text.AppendOutline(path[:0], run.Text, run.Size)
		translate(path, Point{X: origin.X + run.X, Y: origin.Y + run.Y})
		if len(path) > 0 && !visit(path) {
			return
		}
	}
}
