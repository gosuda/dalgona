//go:build gui

package panel

import (
	"image"
	"image/color"

	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

type Kind uint8

const (
	Point Kind = iota
	Secondary
	Quiet
	Destructive
)

type Button struct {
	widget.Clickable
	Busy     string
	Disabled bool
}

func (b *Button) Clicked(gtx layout.Context) bool {
	clicked := b.Clickable.Clicked(gtx)
	return clicked && b.Busy == "" && !b.Disabled
}

type buttonPaint struct {
	fill, edge, ink color.NRGBA
	padding         theme.Dp
}

func (b *Button) paint(env shell.Env, kind Kind) buttonPaint {
	p := env.Theme.Palette()
	if b.Busy != "" || b.Disabled {
		return buttonPaint{fill: p.Tone, edge: p.Line, ink: p.Dim, padding: theme.Space5}
	}
	switch kind {
	case Point:
		hue := env.Theme.Point(env.Thread.Thread)
		return buttonPaint{fill: hue.Fill, edge: hue.Fill, ink: hue.Ink, padding: theme.Space5}
	case Secondary:
		return buttonPaint{fill: p.Tone, edge: p.Strong, ink: p.Text, padding: theme.Space5}
	case Destructive:
		return buttonPaint{edge: p.FailedText, ink: p.FailedText, padding: theme.Space5}
	case Quiet:
	}
	return buttonPaint{ink: p.Dim, padding: theme.Space3}
}

func (b *Button) Layout(gtx layout.Context, f *Frame, env shell.Env, label string, kind Kind) layout.Dimensions {
	shown := label
	if b.Busy != "" {
		shown = b.Busy
	}
	look := b.paint(env, kind)
	inactive := b.Busy != "" || b.Disabled
	if !inactive {
		f.Focusable(&b.Clickable)
	}
	dims := b.Clickable.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.Button.Add(gtx.Ops)
		semantic.LabelOp(shown).Add(gtx.Ops)
		semantic.EnabledOp(!inactive).Add(gtx.Ops)
		if b.Busy != "" {
			semantic.DescriptionOp("Working").Add(gtx.Ops)
		}
		return b.face(gtx, env, look, shown)
	})
	access.Add(gtx, image.Rectangle{Max: dims.Size}, platform.SemanticNode{Role: platform.RoleButton, Name: shown, Disabled: inactive, Reason: b.Busy, Action: "activate"}, access.Actions{Click: &b.Clickable})
	return dims
}

func (b *Button) face(gtx layout.Context, env shell.Env, look buttonPaint, shown string) layout.Dimensions {
	pad := dp(gtx, look.padding)
	inner := gtx
	inner.Constraints.Min = image.Point{}
	inner.Constraints.Max.X = max(0, gtx.Constraints.Max.X-2*pad)
	call, dims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions {
		return paintText(gtx, env.Shaper, controlText(look.ink), shown)
	})
	size := image.Pt(dims.Size.X+2*pad, max(dp(gtx, theme.ControlHeight), dims.Size.Y))
	radius := dp(gtx, theme.RadiusControl)
	r := image.Rectangle{Max: size}
	if look.fill.A > 0 {
		giodraw.FillRRect(gtx, look.fill, r, radius)
	}
	if look.edge.A > 0 {
		giodraw.StrokeRRect(gtx, look.edge, r, radius, dp(gtx, theme.BorderWidth))
	}
	giodraw.Place(gtx, image.Pt(pad, (size.Y-dims.Size.Y)/2), call)
	if gtx.Focused(&b.Clickable) {
		focusRing(gtx, env.Theme.Palette().Text, size, radius)
	}
	return layout.Dimensions{Size: size}
}

type Choice struct {
	widget.Clickable
}

type Option struct {
	Label       string
	Description string
	Marker      string
	Selected    bool
	Disabled    bool
}

func (c *Choice) Layout(gtx layout.Context, f *Frame, env shell.Env, o Option) layout.Dimensions {
	return c.mark(gtx, f, env, o, semantic.RadioButton)
}

type Check struct {
	widget.Clickable
}

func (c *Check) Layout(gtx layout.Context, f *Frame, env shell.Env, o Option) layout.Dimensions {
	return (*Choice)(c).mark(gtx, f, env, o, semantic.CheckBox)
}

func (c *Check) Clicked(gtx layout.Context) bool { return c.Clickable.Clicked(gtx) }

func (c *Choice) Item(gtx layout.Context, f *Frame, env shell.Env, o Option) layout.Dimensions {
	return c.mark(gtx, f, env, o, semantic.Button)
}

func (c *Choice) mark(gtx layout.Context, f *Frame, env shell.Env, o Option, class semantic.ClassOp) layout.Dimensions {
	if !o.Disabled {
		f.Focusable(&c.Clickable)
	}
	dims := c.Clickable.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		class.Add(gtx.Ops)
		semantic.LabelOp(optionName(o)).Add(gtx.Ops)
		semantic.SelectedOp(o.Selected).Add(gtx.Ops)
		semantic.EnabledOp(!o.Disabled).Add(gtx.Ops)
		if o.Description != "" {
			semantic.DescriptionOp(o.Description).Add(gtx.Ops)
		}
		return c.row(gtx, env, o, class)
	})
	role := platform.RoleButton
	switch class {
	case semantic.RadioButton:
		role = platform.RoleRadioButton
	case semantic.CheckBox:
		role = platform.RoleCheckBox
	case semantic.Unknown, semantic.Button, semantic.Editor, semantic.Switch:
	}
	n := platform.SemanticNode{Role: role, Name: optionName(o), Description: o.Description, Disabled: o.Disabled, Action: "select"}
	if role == platform.RoleButton {
		n.Selectable, n.Selected = true, o.Selected
	} else {
		n.Checked = o.Selected
	}
	access.Add(gtx, image.Rectangle{Max: dims.Size}, n, access.Actions{Click: &c.Clickable})
	return dims
}

func optionName(o Option) string {
	if o.Marker == "" {
		return o.Label
	}
	return o.Label + " " + o.Marker
}

func (c *Choice) row(gtx layout.Context, env shell.Env, o Option, class semantic.ClassOp) layout.Dimensions {
	p := env.Theme.Palette()
	column := dp(gtx, theme.OptionMarkColumn) + dp(gtx, theme.OptionGap)
	if class == semantic.Button {
		column = 0
	}
	pad := dp(gtx, theme.OptionPaddingBlock)
	inner := gtx
	inner.Constraints.Min = image.Point{}
	inner.Constraints.Max.X = max(0, gtx.Constraints.Max.X-column)
	call, dims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions { return optionText(gtx, env, o) })
	height := max(dims.Size.Y+2*pad, dp(gtx, theme.TargetMin))
	size := image.Pt(gtx.Constraints.Max.X, height)
	if class != semantic.Button {
		markSize := dp(gtx, theme.OptionMarkSize)
		c.drawMark(gtx, p, image.Pt(0, pad+(min(dims.Size.Y, markSize*2)-markSize)/2), markShape{size: markSize, filled: o.Selected, round: class == semantic.RadioButton})
	}
	giodraw.Place(gtx, image.Pt(column, pad), call)
	if gtx.Focused(&c.Clickable) {
		focusRing(gtx, p.Text, size, dp(gtx, theme.RadiusTag))
	}
	return layout.Dimensions{Size: size}
}

type markShape struct {
	size   int
	filled bool
	round  bool
}

func (c *Choice) drawMark(gtx layout.Context, p theme.Palette, at image.Point, m markShape) {
	radius := dp(gtx, theme.RadiusEdit)
	if m.round {
		radius = m.size / 2
	}
	r := image.Rectangle{Min: at, Max: at.Add(image.Pt(m.size, m.size))}
	giodraw.StrokeRRect(gtx, p.Strong, r, radius, dp(gtx, theme.BorderWidth))
	if !m.filled {
		return
	}
	inset := m.size / 4
	dot := image.Rectangle{Min: r.Min.Add(image.Pt(inset, inset)), Max: r.Max.Sub(image.Pt(inset, inset))}
	giodraw.FillRRect(gtx, p.Text, dot, max(0, radius-inset))
}

func optionText(gtx layout.Context, env shell.Env, o Option) layout.Dimensions {
	p := env.Theme.Palette()
	label := bodyText(p)
	if o.Disabled {
		label.color = p.Dim
	}
	children := []layout.FlexChild{layout.Rigid(func(gtx layout.Context) layout.Dimensions {
		return layout.Flex{Alignment: layout.Baseline}.Layout(gtx,
			layout.Flexed(1, func(gtx layout.Context) layout.Dimensions { return paintText(gtx, env.Shaper, label, o.Label) }),
			layout.Rigid(func(gtx layout.Context) layout.Dimensions { return marker(gtx, env, o.Marker) }),
		)
	})}
	if o.Description != "" {
		children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return paintText(gtx, env.Shaper, noteText(p), o.Description)
		}))
	}
	return layout.Flex{Axis: layout.Vertical}.Layout(gtx, children...)
}

func marker(gtx layout.Context, env shell.Env, text string) layout.Dimensions {
	if text == "" {
		return layout.Dimensions{}
	}
	return layout.Inset{Left: dpUnit(theme.Space3)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		st := noteText(env.Theme.Palette())
		st.size = theme.SizeCaption
		return paintText(gtx, env.Shaper, st, text)
	})
}
