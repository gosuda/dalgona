//go:build gui

package shell

import (
	"image"
	"image/color"

	giofont "gioui.org/font"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op/clip"
	"gioui.org/op/paint"
	"gioui.org/text"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

type textStyle struct {
	face     giofont.Typeface
	weight   theme.Weight
	size     theme.Sp
	leading  float32
	color    color.NRGBA
	maxLines int
	align    text.Alignment
}

func dp(gtx layout.Context, v theme.Dp) int {
	return gtx.Dp(logical(v))
}

func logical(v theme.Dp) unit.Dp {
	return unit.Dp(v)
}

func font(st textStyle) giofont.Font {
	return giofont.Font{Typeface: st.face, Weight: platform.FontWeight(st.weight)}
}

func paintText(gtx layout.Context, shaper *text.Shaper, st textStyle, txt string) layout.Dimensions {
	l := widget.Label{MaxLines: st.maxLines, LineHeightScale: st.leading, Alignment: st.align}
	return l.Layout(gtx, shaper, font(st), unit.Sp(st.size), txt, giodraw.Material(gtx, st.color))
}

func drawText(gtx layout.Context, shaper *text.Shaper, st textStyle, txt string) layout.Dimensions {
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions { return paintText(gtx, shaper, st, txt) })
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	call.Add(gtx.Ops)
	return dims
}

func labelled(gtx layout.Context, label string, w layout.Widget) layout.Dimensions {
	call, dims := giodraw.Record(gtx, w)
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	semantic.LabelOp(label).Add(gtx.Ops)
	call.Add(gtx.Ops)
	return dims
}

func landmark(gtx layout.Context, label string, ground color.NRGBA, w layout.Widget) layout.Dimensions {
	call, dims := giodraw.Record(gtx, w)
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	if label != "" {
		semantic.LabelOp(label).Add(gtx.Ops)
	}
	if ground.A > 0 {
		paint.FillShape(gtx.Ops, ground, clip.Rect{Max: dims.Size}.Op())
	}
	call.Add(gtx.Ops)
	return dims
}

func focusRing(gtx layout.Context, c color.NRGBA, size image.Point, radius int) {
	offset := dp(gtx, theme.FocusOffset)
	width := dp(gtx, theme.FocusWidth)
	grow := offset + width/2
	r := image.Rectangle{Min: image.Pt(-grow, -grow), Max: size.Add(image.Pt(grow, grow))}
	giodraw.StrokeRRect(gtx, c, r, radius+grow, width)
}

func rule(gtx layout.Context, c color.NRGBA, width, y int) {
	giodraw.FillRect(gtx, c, image.Rect(0, y, width, y+dp(gtx, theme.BorderWidth)))
}
