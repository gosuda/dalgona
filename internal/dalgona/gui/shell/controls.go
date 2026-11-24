//go:build gui

package shell

import (
	"image"
	"image/color"

	"gioui.org/io/semantic"
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
	quiet variant = iota
	secondary
	pointed
	destructive
)

type button struct {
	click      *widget.Clickable
	label      string
	variant    variant
	busy       bool
	hue        theme.Hue
	note       string
	id         platform.NodeID
	role       platform.Role
	expandable bool
	expanded   bool
}

type buttonLook struct {
	fill   color.NRGBA
	edge   color.NRGBA
	ink    color.NRGBA
	inline theme.Dp
}

func (b button) look(p theme.Palette) buttonLook {
	switch {
	case b.busy:
		return buttonLook{fill: p.Tone, edge: p.Line, ink: p.Dim, inline: theme.Space5}
	case b.variant == pointed:
		return buttonLook{fill: b.hue.Fill, edge: b.hue.Fill, ink: b.hue.Ink, inline: theme.Space5}
	case b.variant == secondary:
		return buttonLook{fill: p.Tone, edge: p.Strong, ink: p.Text, inline: theme.Space5}
	case b.variant == destructive:
		return buttonLook{edge: p.FailedText, ink: p.FailedText, inline: theme.Space5}
	}
	return buttonLook{ink: p.Dim, inline: theme.Space3}
}

func drawButton(gtx layout.Context, shaper *text.Shaper, p theme.Palette, b button) layout.Dimensions {
	look := b.look(p)
	dims := b.click.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.Button.Add(gtx.Ops)
		semantic.LabelOp(b.label).Add(gtx.Ops)
		semantic.EnabledOp(!b.busy).Add(gtx.Ops)
		if b.note != "" {
			semantic.DescriptionOp(b.note).Add(gtx.Ops)
		}
		st := textStyle{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeLabel, leading: theme.LeadingTight, color: look.ink, maxLines: 1}
		inline := dp(gtx, look.inline)
		textGtx := gtx
		textGtx.Constraints.Min = image.Point{}
		textGtx.Constraints.Max.X = max(0, gtx.Constraints.Max.X-2*inline)
		call, dims := giodraw.Record(textGtx, func(gtx layout.Context) layout.Dimensions { return paintText(gtx, shaper, st, b.label) })
		size := image.Pt(dims.Size.X+2*inline, max(dp(gtx, theme.ControlHeight), dims.Size.Y))
		radius := dp(gtx, theme.RadiusControl)
		bounds := image.Rectangle{Max: size}
		if look.fill.A > 0 {
			giodraw.FillRRect(gtx, look.fill, bounds, radius)
		}
		if look.edge.A > 0 {
			giodraw.StrokeRRect(gtx, look.edge, bounds, radius, dp(gtx, theme.BorderWidth))
		}
		giodraw.Place(gtx, image.Pt(inline, (size.Y-dims.Size.Y)/2), call)
		if gtx.Focused(b.click) {
			focusRing(gtx, p.Text, size, radius)
		}
		return layout.Dimensions{Size: size}
	})
	access.Add(gtx, image.Rectangle{Max: dims.Size}, b.semantic(), access.Actions{Click: b.click})
	return dims
}

func (b button) semantic() platform.SemanticNode {
	role := b.role
	if role == 0 {
		role = platform.RoleButton
	}
	return platform.SemanticNode{ID: b.id, Role: role, Name: b.label, Description: b.note, Disabled: b.busy, Action: "activate", Expandable: b.expandable, Expanded: b.expanded}
}

func pressed(gtx layout.Context, c *widget.Clickable, busy bool) bool {
	clicked := c.Clicked(gtx)
	return clicked && !busy
}

func row(gtx layout.Context, gap theme.Dp, items ...layout.Widget) layout.Dimensions {
	children := make([]layout.FlexChild, 0, 2*len(items))
	for i, w := range items {
		if i > 0 {
			children = append(children, layout.Rigid(layout.Spacer{Width: logical(gap)}.Layout))
		}
		children = append(children, layout.Rigid(w))
	}
	return layout.Flex{Axis: layout.Horizontal, Alignment: layout.Middle}.Layout(gtx, children...)
}

func wrapRow(gtx layout.Context, gap theme.Dp, items ...layout.Widget) layout.Dimensions {
	space := dp(gtx, gap)
	room := gtx.Constraints.Max.X
	child := gtx
	child.Constraints.Min = image.Point{}
	x, y, line, width := 0, 0, 0, 0
	for _, w := range items {
		call, dims := giodraw.Record(child, w)
		if x > 0 && x+dims.Size.X > room {
			x, y, line = 0, y+line+space, 0
		}
		giodraw.Place(gtx, image.Pt(x, y), call)
		x += dims.Size.X + space
		line = max(line, dims.Size.Y)
		width = max(width, x-space)
	}
	return layout.Dimensions{Size: image.Pt(width, y+line)}
}
