//go:build gui

package giodraw

import (
	"image"
	"image/color"

	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/op/paint"
)

func Material(gtx layout.Context, c color.NRGBA) op.CallOp {
	m := op.Record(gtx.Ops)
	paint.ColorOp{Color: c}.Add(gtx.Ops)
	return m.Stop()
}

func Record(gtx layout.Context, w layout.Widget) (op.CallOp, layout.Dimensions) {
	m := op.Record(gtx.Ops)
	dims := w(gtx)
	return m.Stop(), dims
}

func Place(gtx layout.Context, at image.Point, call op.CallOp) {
	defer op.Offset(at).Push(gtx.Ops).Pop()
	call.Add(gtx.Ops)
}

func FillRect(gtx layout.Context, c color.NRGBA, r image.Rectangle) {
	paint.FillShape(gtx.Ops, c, clip.Rect(r).Op())
}

func FillRRect(gtx layout.Context, c color.NRGBA, r image.Rectangle, radius int) {
	paint.FillShape(gtx.Ops, c, clip.UniformRRect(r, radius).Op(gtx.Ops))
}

func StrokeRRect(gtx layout.Context, c color.NRGBA, r image.Rectangle, radius, width int) {
	paint.FillShape(gtx.Ops, c, clip.Stroke{Path: clip.UniformRRect(r, radius).Path(gtx.Ops), Width: float32(width)}.Op())
}
