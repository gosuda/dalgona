//go:build gui

package panel

import (
	"image"
	"image/color"

	giofont "gioui.org/font"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op/clip"
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

func bodyText(p theme.Palette) textStyle {
	return textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeBody, leading: theme.LeadingBody, color: p.Text}
}

func noteText(p theme.Palette) textStyle {
	return textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingDescription, color: p.Dim}
}

func failureText(p theme.Palette) textStyle {
	return textStyle{face: platform.ProseFace, weight: theme.WeightStrong, size: theme.SizeBody, leading: theme.LeadingBody, color: p.FailedText}
}

func legendText(p theme.Palette) textStyle {
	return textStyle{face: platform.TitleFace, weight: theme.WeightTitle, size: theme.SizeLabel, leading: theme.LeadingLabel, color: p.Text}
}

func titleText(p theme.Palette) textStyle {
	return textStyle{face: platform.TitleFace, weight: theme.WeightTitle, size: theme.SizeTitle, leading: theme.LeadingCardTitle, color: p.Text}
}

func controlText(c color.NRGBA) textStyle {
	return textStyle{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeLabel, leading: theme.LeadingTight, color: c, maxLines: 1}
}

func codeText(p theme.Palette) textStyle {
	return textStyle{face: platform.CodeFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingCode, color: p.Text}
}

func dp(gtx layout.Context, v theme.Dp) int {
	return gtx.Dp(unit.Dp(v))
}

func dpUnit(v theme.Dp) unit.Dp {
	return unit.Dp(v)
}

func paintText(gtx layout.Context, shaper *text.Shaper, st textStyle, txt string) layout.Dimensions {
	l := widget.Label{MaxLines: st.maxLines, LineHeightScale: st.leading, Alignment: st.align}
	f := giofont.Font{Typeface: st.face, Weight: platform.FontWeight(st.weight)}
	return l.Layout(gtx, shaper, f, unit.Sp(st.size), txt, giodraw.Material(gtx, st.color))
}

func drawText(gtx layout.Context, shaper *text.Shaper, st textStyle, txt string) layout.Dimensions {
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions { return paintText(gtx, shaper, st, txt) })
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	call.Add(gtx.Ops)
	return dims
}

func announced(gtx layout.Context, shaper *text.Shaper, st textStyle, txt string) layout.Dimensions {
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions { return paintText(gtx, shaper, st, txt) })
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	semantic.DescriptionOp("alert").Add(gtx.Ops)
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

func (s *scene) text(st textStyle, txt string) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions { return drawText(gtx, s.env.Shaper, st, txt) }
}

func (s *scene) para(txt string) layout.Widget { return s.text(bodyText(s.env.Theme.Palette()), txt) }

func (s *scene) note(txt string) layout.Widget { return s.text(noteText(s.env.Theme.Palette()), txt) }

func (s *scene) legend(txt string) layout.Widget {
	return s.text(legendText(s.env.Theme.Palette()), txt)
}

func (s *scene) code(txt string) layout.Widget { return s.text(codeText(s.env.Theme.Palette()), txt) }

func (s *scene) alert(txt string) layout.Widget {
	st := failureText(s.env.Theme.Palette())
	return func(gtx layout.Context) layout.Dimensions { return announced(gtx, s.env.Shaper, st, txt) }
}

const themeOptionIndent = theme.OptionMarkColumn + theme.OptionGap
