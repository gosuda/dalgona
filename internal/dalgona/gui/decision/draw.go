//go:build gui

package decision

import (
	"image"
	"image/color"

	giofont "gioui.org/font"
	"gioui.org/layout"
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
	wrap     text.WrapPolicy
}

func dp(gtx layout.Context, v theme.Dp) int { return gtx.Dp(unit.Dp(v)) }

func drawText(gtx layout.Context, shaper *text.Shaper, st textStyle, txt string) layout.Dimensions {
	l := widget.Label{MaxLines: st.maxLines, LineHeightScale: st.leading, WrapPolicy: st.wrap}
	face := giofont.Font{Typeface: st.face, Weight: platform.FontWeight(st.weight)}
	return l.Layout(gtx, shaper, face, unit.Sp(st.size), txt, giodraw.Material(gtx, st.color))
}

func prose(c color.NRGBA) textStyle {
	return textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeBody, leading: theme.LeadingReason, color: c}
}

func small(c color.NRGBA) textStyle {
	return textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingDescription, color: c}
}

func label(c color.NRGBA) textStyle {
	return textStyle{face: platform.ProseFace, weight: theme.WeightTitle, size: theme.SizeCaption, leading: theme.LeadingLabel, color: c}
}

func mono(c color.NRGBA, size theme.Sp) textStyle {
	return textStyle{face: platform.CodeFace, weight: theme.WeightText, size: size, leading: theme.LeadingCode, color: c, wrap: text.WrapGraphemes}
}

func focusRing(gtx layout.Context, c color.NRGBA, size image.Point, radius int) {
	grow := dp(gtx, theme.FocusOffset) + dp(gtx, theme.FocusWidth)/2
	r := image.Rectangle{Min: image.Pt(-grow, -grow), Max: size.Add(image.Pt(grow, grow))}
	giodraw.StrokeRRect(gtx, c, r, radius+grow, dp(gtx, theme.FocusWidth))
}

func column(gtx layout.Context, gap theme.Dp, items ...layout.Widget) layout.Dimensions {
	children := make([]layout.FlexChild, 0, 2*len(items))
	for _, w := range items {
		if w == nil {
			continue
		}
		if len(children) > 0 {
			children = append(children, layout.Rigid(layout.Spacer{Height: unit.Dp(gap)}.Layout))
		}
		children = append(children, layout.Rigid(w))
	}
	return layout.Flex{Axis: layout.Vertical}.Layout(gtx, children...)
}

func sized(gtx layout.Context) layout.Context {
	gtx.Constraints.Min = image.Point{}
	return gtx
}

func selectionTint(p theme.Palette) color.NRGBA {
	c := p.Strong
	c.A = 0x66
	return c
}
