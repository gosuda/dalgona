//go:build gui

package decision

import (
	"image"
	"image/color"

	"gioui.org/io/key"
	"gioui.org/layout"
	"gioui.org/text"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

type variant uint8

const (
	variantDecision variant = iota
	variantPoint
	variantQuiet
	variantDestructive
	variantSecondary
)

type look struct {
	fill, edge, ink color.NRGBA
	inline          theme.Dp
}

func lookOf(v variant, disabled bool, p theme.Palette, point theme.Hue) look {
	switch {
	case disabled:
		return look{fill: p.Tone, edge: p.Line, ink: p.Dim, inline: theme.DecisionPadding}
	case v == variantPoint:
		return look{fill: point.Fill, edge: point.Fill, ink: point.Ink, inline: theme.DecisionPadding}
	case v == variantQuiet:
		return look{ink: p.Dim, inline: theme.Space3}
	case v == variantDestructive:
		return look{edge: p.FailedText, ink: p.FailedText, inline: theme.DecisionPadding}
	case v == variantSecondary:
		return look{fill: p.Tone, edge: p.Strong, ink: p.Text, inline: theme.DecisionPadding}
	}
	return look{edge: p.Strong, ink: p.Text, inline: theme.DecisionPadding}
}

type shortcut struct {
	glyph string
	name  key.Name
	about string
}

var (
	shortcutAccept  = shortcut{glyph: "y", name: "Y", about: "Shortcut y"}
	shortcutSession = shortcut{glyph: "a", name: "A", about: "Shortcut a"}
	shortcutDecline = shortcut{glyph: "n", name: "N", about: "Shortcut n"}
	shortcutEscape  = shortcut{glyph: "esc", name: key.NameEscape, about: "Shortcut Escape"}
)

type button struct {
	click   *widget.Clickable
	label   string
	key     shortcut
	variant variant
	reason  string
	height  theme.Dp
	size    theme.Sp
	stretch bool
}

type paints struct {
	shaper  *text.Shaper
	palette theme.Palette
	point   theme.Hue
}

func (b button) layout(gtx layout.Context, ps paints) layout.Dimensions {
	disabled := b.reason != ""
	if disabled {
		gtx = gtx.Disabled()
	}
	return b.click.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		n := platform.SemanticNode{Role: platform.RoleButton, Name: b.label, Description: b.key.about, Reason: b.reason, Disabled: disabled, Action: "activate"}
		n.Add(gtx.Ops)
		dims := b.face(gtx, ps, lookOf(b.variant, disabled, ps.palette, ps.point))
		access.Add(gtx, image.Rectangle{Max: dims.Size}, n, access.Actions{Click: b.click})
		return dims
	})
}

func (b button) face(gtx layout.Context, ps paints, lk look) layout.Dimensions {
	pad, gap := dp(gtx, lk.inline), dp(gtx, theme.Space2)
	inner := sized(gtx)
	keyMacro, keyDims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions { return b.keyGlyph(gtx, ps, lk) })
	keySpan := 0
	if keyDims.Size.X > 0 {
		keySpan = keyDims.Size.X + gap
	}
	inner.Constraints.Max.X = max(0, gtx.Constraints.Max.X-2*pad-keySpan)
	labelMacro, labelDims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions { return b.labelGlyph(gtx, ps, lk) })
	size := b.extent(gtx, labelDims.Size.X+2*pad+keySpan, labelDims.Size.Y)
	b.outline(gtx, ps, lk, size)
	labelAt := image.Pt(pad, (size.Y-labelDims.Size.Y)/2)
	keyAt := image.Pt(size.X-pad-keyDims.Size.X, (size.Y-keyDims.Size.Y)/2)
	if b.variant == variantQuiet {
		labelAt.X = (size.X - labelDims.Size.X - keySpan) / 2
		keyAt.X = labelAt.X + labelDims.Size.X + gap
		underline(gtx, lk.ink, labelAt, labelDims)
	}
	giodraw.Place(gtx, labelAt, labelMacro)
	giodraw.Place(gtx, keyAt, keyMacro)
	return layout.Dimensions{Size: size}
}

func (b button) labelGlyph(gtx layout.Context, ps paints, lk look) layout.Dimensions {
	return drawText(gtx, ps.shaper, textStyle{face: platform.TitleFace, weight: theme.WeightTitle, size: b.size, leading: theme.LeadingTight, color: lk.ink, maxLines: 1}, b.label)
}

func (b button) keyGlyph(gtx layout.Context, ps paints, lk look) layout.Dimensions {
	if b.key.glyph == "" {
		return layout.Dimensions{}
	}
	return drawText(gtx, ps.shaper, textStyle{face: platform.CodeFace, weight: theme.WeightText, size: theme.SizeCaption, leading: theme.LeadingTight, color: lk.ink, maxLines: 1}, b.key.glyph)
}

func (b button) extent(gtx layout.Context, width, height int) image.Point {
	size := image.Pt(width, max(dp(gtx, b.height), height))
	if b.stretch {
		size.X = gtx.Constraints.Max.X
	}
	size.X = max(size.X, gtx.Constraints.Min.X)
	return size
}

func (b button) outline(gtx layout.Context, ps paints, lk look, size image.Point) {
	radius := dp(gtx, theme.RadiusDecision)
	bounds := image.Rectangle{Max: size}
	if lk.fill.A > 0 {
		giodraw.FillRRect(gtx, lk.fill, bounds, radius)
	}
	if lk.edge.A > 0 {
		giodraw.StrokeRRect(gtx, lk.edge, bounds, radius, dp(gtx, theme.BorderWidth))
	}
	if gtx.Focused(b.click) {
		focusRing(gtx, ps.palette.Text, size, radius)
	}
}

func underline(gtx layout.Context, c color.NRGBA, at image.Point, dims layout.Dimensions) {
	y := at.Y + dims.Size.Y - dims.Baseline + dp(gtx, theme.Space1)
	giodraw.FillRect(gtx, c, image.Rect(at.X, y, at.X+dims.Size.X, y+dp(gtx, theme.BorderWidth)))
}

func (b button) natural(gtx layout.Context, ps paints) int {
	lk := lookOf(b.variant, false, ps.palette, ps.point)
	free := sized(gtx)
	free.Constraints.Max.X = 1 << 20
	_, labelDims := giodraw.Record(free, func(gtx layout.Context) layout.Dimensions { return b.labelGlyph(gtx, ps, lk) })
	_, keyDims := giodraw.Record(free, func(gtx layout.Context) layout.Dimensions { return b.keyGlyph(gtx, ps, lk) })
	width := labelDims.Size.X + 2*dp(gtx, lk.inline)
	if keyDims.Size.X > 0 {
		width += keyDims.Size.X + dp(gtx, theme.Space2)
	}
	return width
}

func decision(click *widget.Clickable, name string, keys shortcut, v variant) button {
	b := button{click: click, label: name, key: keys, variant: v, height: theme.DecisionHeight, size: theme.SizeSmall}
	switch v {
	case variantPoint:
		b.height, b.size = theme.PrimaryDecisionHeight, theme.SizeBody
	case variantQuiet:
		b.height = theme.ControlHeight
	case variantDecision, variantDestructive, variantSecondary:
	}
	return b
}
