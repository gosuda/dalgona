//go:build gui

package math

import (
	"image/color"

	"gioui.org/f32"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/op/paint"
	"github.com/go-text/typesetting/font/opentype"
)

func (l *Layout) Draw(ops *op.Ops, ink color.NRGBA) {
	l.outline(func(path []Segment) bool {
		fillOutline(ops, path, ink)
		return true
	})
}

func pt(p Point) f32.Point {
	return f32.Pt(p.X, p.Y)
}

func fillOutline(ops *op.Ops, segments []Segment, ink color.NRGBA) {
	var path clip.Path
	path.Begin(ops)
	for _, segment := range segments {
		p := segment.Points
		switch segment.Op {
		case opentype.SegmentOpMoveTo:
			path.MoveTo(pt(p[0]))
		case opentype.SegmentOpLineTo:
			path.LineTo(pt(p[0]))
		case opentype.SegmentOpQuadTo:
			path.QuadTo(pt(p[0]), pt(p[1]))
		case opentype.SegmentOpCubeTo:
			path.CubeTo(pt(p[0]), pt(p[1]), pt(p[2]))
		}
	}
	paint.FillShape(ops, ink, clip.Outline{Path: path.End()}.Op())
}
