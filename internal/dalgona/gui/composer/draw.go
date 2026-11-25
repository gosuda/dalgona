//go:build gui

package composer

import (
	"image"
	"image/color"

	giofont "gioui.org/font"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/text"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
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
}

func dp(gtx layout.Context, v theme.Dp) int { return gtx.Dp(unit.Dp(v)) }

func drawText(gtx layout.Context, shaper *text.Shaper, st textStyle, txt string) layout.Dimensions {
	l := widget.Label{MaxLines: st.maxLines, LineHeightScale: st.leading}
	face := giofont.Font{Typeface: st.face, Weight: platform.FontWeight(st.weight)}
	return l.Layout(gtx, shaper, face, unit.Sp(st.size), txt, giodraw.Material(gtx, st.color))
}

func caption(c color.NRGBA) textStyle {
	return textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeCaption, leading: theme.LeadingLabel, color: c}
}

func small(c color.NRGBA) textStyle {
	return textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingDescription, color: c}
}

func focusRing(gtx layout.Context, c color.NRGBA, size image.Point, radius int) {
	grow := dp(gtx, theme.FocusOffset) + dp(gtx, theme.FocusWidth)/2
	r := image.Rectangle{Min: image.Pt(-grow, -grow), Max: size.Add(image.Pt(grow, grow))}
	giodraw.StrokeRRect(gtx, c, r, radius+grow, dp(gtx, theme.FocusWidth))
}

type variant uint8

const (
	variantSecondary variant = iota
	variantPoint
	variantQuiet
	variantDestructive
)

type look struct {
	fill, edge, ink color.NRGBA
	inline          theme.Dp
}

func lookOf(v variant, disabled bool, p theme.Palette, point theme.Hue) look {
	switch {
	case disabled:
		return look{fill: p.Tone, edge: p.Line, ink: p.Dim, inline: theme.Space5}
	case v == variantPoint:
		return look{fill: point.Fill, edge: point.Fill, ink: point.Ink, inline: theme.Space5}
	case v == variantQuiet:
		return look{ink: p.Dim, inline: theme.Space3}
	case v == variantDestructive:
		return look{edge: p.FailedText, ink: p.FailedText, inline: theme.Space5}
	}
	return look{fill: p.Tone, edge: p.Strong, ink: p.Text, inline: theme.Space5}
}

type control struct {
	click    *widget.Clickable
	label    string
	describe string
	reason   string
	variant  variant
	height   theme.Dp
}

type dockLook struct {
	shaper  *text.Shaper
	palette theme.Palette
	point   theme.Hue
}

func region(gtx layout.Context, label string, call op.CallOp, dims layout.Dimensions) {
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	semantic.LabelOp(label).Add(gtx.Ops)
	call.Add(gtx.Ops)
}

func (c control) layout(gtx layout.Context, shaper *text.Shaper, p theme.Palette, point theme.Hue) layout.Dimensions {
	return c.click.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		n := platform.SemanticNode{Role: platform.RoleButton, Name: c.label, Description: c.describe, Reason: c.reason, Disabled: c.reason != "", Action: "activate"}
		n.Add(gtx.Ops)
		lk := lookOf(c.variant, c.reason != "", p, point)
		inline := dp(gtx, lk.inline)
		textGtx := gtx
		textGtx.Constraints.Min = image.Point{}
		textGtx.Constraints.Max.X = max(0, gtx.Constraints.Max.X-2*inline)
		st := textStyle{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeLabel, leading: theme.LeadingTight, color: lk.ink, maxLines: 1}
		call, dims := giodraw.Record(textGtx, func(gtx layout.Context) layout.Dimensions { return drawText(gtx, shaper, st, c.label) })
		height := c.height
		if height == 0 {
			height = theme.ControlHeight
		}
		size := image.Pt(dims.Size.X+2*inline, max(dp(gtx, height), dims.Size.Y))
		radius := dp(gtx, theme.RadiusControl)
		bounds := image.Rectangle{Max: size}
		if lk.fill.A > 0 {
			giodraw.FillRRect(gtx, lk.fill, bounds, radius)
		}
		if lk.edge.A > 0 {
			giodraw.StrokeRRect(gtx, lk.edge, bounds, radius, dp(gtx, theme.BorderWidth))
		}
		giodraw.Place(gtx, image.Pt(inline, (size.Y-dims.Size.Y)/2), call)
		access.Add(gtx, bounds, n, access.Actions{Click: c.click})
		if gtx.Focused(c.click) {
			focusRing(gtx, p.Text, size, radius)
		}
		return layout.Dimensions{Size: size}
	})
}

func flow(gtx layout.Context, gap theme.Dp, items ...layout.Widget) layout.Dimensions {
	space, width := dp(gtx, gap), gtx.Constraints.Max.X
	var at image.Point
	lineHeight, used := 0, 0
	for _, w := range items {
		if w == nil {
			continue
		}
		itemGtx := gtx
		itemGtx.Constraints.Min = image.Point{}
		call, dims := giodraw.Record(itemGtx, w)
		if at.X > 0 && at.X+dims.Size.X > width {
			at = image.Pt(0, at.Y+lineHeight+space)
			lineHeight = 0
		}
		giodraw.Place(gtx, at, call)
		at.X += dims.Size.X + space
		lineHeight = max(lineHeight, dims.Size.Y)
		used = max(used, at.X-space)
	}
	if used == 0 {
		return layout.Dimensions{}
	}
	return layout.Dimensions{Size: image.Pt(min(width, used), at.Y+lineHeight)}
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
