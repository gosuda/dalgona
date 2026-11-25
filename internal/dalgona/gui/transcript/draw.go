//go:build gui

package transcript

import (
	"image"
	"image/color"

	giofont "gioui.org/font"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/op/paint"
	"gioui.org/text"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document/code"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

type style struct {
	face    giofont.Typeface
	weight  theme.Weight
	size    theme.Sp
	leading float32
	color   color.NRGBA
	lines   int
}

func dp(gtx layout.Context, v theme.Dp) int {
	return gtx.Dp(unit.Dp(v))
}

func prose(c color.NRGBA) style {
	return style{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeBody, leading: theme.LeadingBody, color: c}
}

func caption(c color.NRGBA) style {
	return style{face: platform.TitleFace, weight: theme.WeightTitle, size: theme.SizeSmall, leading: theme.LeadingLabel, color: c}
}

func mono(c color.NRGBA) style {
	return style{face: platform.CodeFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingCode, color: c}
}

func paintLabel(gtx layout.Context, shaper *text.Shaper, st style, txt string) layout.Dimensions {
	rec := op.Record(gtx.Ops)
	paint.ColorOp{Color: st.color}.Add(gtx.Ops)
	material := rec.Stop()
	l := widget.Label{MaxLines: st.lines, LineHeightScale: st.leading}
	face := giofont.Font{Typeface: st.face, Weight: platform.FontWeight(st.weight)}
	return l.Layout(gtx, shaper, face, unit.Sp(st.size), txt, material)
}

func label(gtx layout.Context, shaper *text.Shaper, st style, txt string) layout.Dimensions {
	macro := op.Record(gtx.Ops)
	dims := paintLabel(gtx, shaper, st, txt)
	call := macro.Stop()
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	call.Add(gtx.Ops)
	return dims
}

func focusRing(gtx layout.Context, c color.NRGBA, size image.Point, radius int) {
	grow := dp(gtx, theme.FocusOffset) + dp(gtx, theme.FocusWidth)/2
	r := image.Rectangle{Min: image.Pt(-grow, -grow), Max: size.Add(image.Pt(grow, grow))}
	giodraw.StrokeRRect(gtx, c, r, radius+grow, dp(gtx, theme.FocusWidth))
}

type control struct {
	shaper  *text.Shaper
	palette theme.Palette
}

func (c control) button(gtx layout.Context, click *widget.Clickable, name string, busy bool) layout.Dimensions {
	return click.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.Button.Add(gtx.Ops)
		semantic.LabelOp(name).Add(gtx.Ops)
		if busy {
			semantic.DescriptionOp("busy").Add(gtx.Ops)
		}
		pad := layout.Inset{Top: unit.Dp(theme.Space1), Bottom: unit.Dp(theme.Space1), Left: unit.Dp(theme.Space2), Right: unit.Dp(theme.Space2)}
		macro := op.Record(gtx.Ops)
		dims := pad.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return paintLabel(gtx, c.shaper, style{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeSmall, leading: theme.LeadingLabel, color: c.palette.Text, lines: 1}, name)
		})
		call := macro.Stop()
		dims.Size.Y = max(dims.Size.Y, dp(gtx, theme.TargetMin))
		radius := dp(gtx, theme.RadiusControl)
		paint.FillShape(gtx.Ops, c.palette.Tone, clip.UniformRRect(image.Rectangle{Max: dims.Size}, radius).Op(gtx.Ops))
		giodraw.StrokeRRect(gtx, c.palette.Strong, image.Rectangle{Max: dims.Size}, radius, dp(gtx, theme.BorderWidth))
		call.Add(gtx.Ops)
		access.Add(gtx, image.Rectangle{Max: dims.Size}, platform.SemanticNode{Role: platform.RoleButton, Name: name, Disabled: busy, Action: "activate"}, access.Actions{Click: click})
		if gtx.Focused(click) {
			focusRing(gtx, c.palette.Text, dims.Size, radius)
		}
		return dims
	})
}

func typography(scale float32) document.Typography {
	return document.Typography{
		Body:       giofont.Font{Typeface: platform.ProseFace},
		Heading:    giofont.Font{Typeface: platform.TitleFace, Weight: platform.FontWeight(theme.WeightTitle)},
		Mono:       giofont.Font{Typeface: platform.CodeFace},
		Size:       unit.Sp(theme.SizeBody),
		CodeSize:   unit.Sp(theme.SizeSmall),
		Headings:   [6]unit.Sp{18, 16, unit.Sp(theme.SizeBody), unit.Sp(theme.SizeBody), unit.Sp(theme.SizeBody), unit.Sp(theme.SizeBody)},
		LineHeight: theme.LeadingBody * scale,
		TabSpaces:  4,
		Indent:     unit.Dp(theme.Space5),
	}
}

func documentPalette(p theme.Palette, text color.NRGBA) document.Palette {
	selection := p.AccentText
	selection.A = 0x48
	pal := document.Palette{Text: text, Muted: p.Dim, Link: p.AccentText, Code: text, CodeBackground: p.Tone, Selection: selection, Rule: p.Line}
	pal.Token = func(kind code.Kind) color.NRGBA { return kind.Color(p) }
	return pal
}

func roleColor(p theme.Palette, role lineRole) color.NRGBA {
	switch role {
	case roleAdd:
		return p.AddedText
	case roleDelete, roleFailed:
		return p.FailedText
	case roleWarning:
		return p.WarningText
	case roleHunk, roleMeta, roleNote, roleDim, roleOmitted:
		return p.Dim
	case roleCurrent:
		return p.AccentText
	case roleText, roleFile, roleStrong:
	}
	return p.Text
}
