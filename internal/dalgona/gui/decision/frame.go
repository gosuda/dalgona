//go:build gui

package decision

import (
	"image"
	"strings"

	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op/clip"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const detailsLabel = "Request details"

type frame struct {
	pending pending
	card    *card
	actions []action
	total   int
	unknown []pending
	focus   bool
	warning *warningNote
}

func (d *Dock) layoutCard(gtx layout.Context, ps paints, f frame) layout.Dimensions {
	budget := gtx.Constraints.Max.Y
	gtx.Constraints.Min = image.Point{}
	return d.outer.Layout(gtx, 1, func(gtx layout.Context, _ int) layout.Dimensions {
		return d.cardFrame(gtx, ps, f, budget)
	})
}

func (d *Dock) cardFrame(gtx layout.Context, ps paints, f frame, budget int) layout.Dimensions {
	pad := dp(gtx, theme.Space5)
	if f.card.details.approval() {
		pad = dp(gtx, theme.Space6)
	}
	width := gtx.Constraints.Max.X
	inner := gtx
	inner.Constraints = layout.Constraints{Max: image.Pt(max(0, width-2*pad), budget)}
	headCall, head := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions { return d.header(gtx, ps, f) })
	footCall, foot := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions { return d.footer(gtx, ps, f) })
	gap := dp(gtx, theme.Space4)
	room := budget - 2*pad - head.Size.Y - foot.Size.Y - 2*gap
	body := inner
	body.Constraints = layout.Constraints{
		Min: image.Pt(inner.Constraints.Max.X, 0),
		Max: image.Pt(inner.Constraints.Max.X, max(room, 2*dp(gtx, theme.ControlHeight))),
	}
	bodyCall, bodyDims := giodraw.Record(body, func(gtx layout.Context) layout.Dimensions { return d.layoutDetails(gtx, ps, f) })
	size := image.Pt(width, 2*pad+head.Size.Y+gap+bodyDims.Size.Y+gap+foot.Size.Y)
	radius := dp(gtx, theme.RadiusCard)
	giodraw.FillRRect(gtx, ps.palette.Surface, image.Rectangle{Max: size}, radius)
	giodraw.StrokeRRect(gtx, ps.palette.Line, image.Rectangle{Max: size}, radius, dp(gtx, theme.BorderWidth))
	giodraw.Place(gtx, image.Pt(pad, pad), headCall)
	giodraw.Place(gtx, image.Pt(pad, pad+head.Size.Y+gap), bodyCall)
	giodraw.Place(gtx, image.Pt(pad, pad+head.Size.Y+gap+bodyDims.Size.Y+gap), footCall)
	return layout.Dimensions{Size: size}
}

func (d *Dock) header(gtx layout.Context, ps paints, f frame) layout.Dimensions {
	kicker := kickerLine(f.card.details.kicker(), f.total, f.pending.endpoint)
	return column(gtx, theme.Space1,
		func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, ps.shaper, label(ps.palette.Dim), kicker)
		},
		func(gtx layout.Context) layout.Dimensions { return d.title(gtx, ps, f, kicker) },
	)
}

func (d *Dock) title(gtx layout.Context, ps paints, f frame, kicker string) layout.Dimensions {
	c := f.card
	heading := c.details.title()
	st := textStyle{face: platform.TitleFace, weight: theme.WeightTitle, size: theme.SizeTitle, leading: theme.LeadingCardTitle, color: ps.palette.Text}
	if !c.details.approval() {
		st.size, st.leading = theme.SizeHeading, theme.LeadingQuestion
	}
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions { return drawText(gtx, ps.shaper, st, heading) })
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	event.Op(gtx.Ops, &c.title)
	for {
		if _, ok := gtx.Event(key.FocusFilter{Target: &c.title}); !ok {
			break
		}
	}
	n := platform.SemanticNode{ID: "heading", Role: platform.RoleHeading, Level: 2, Name: heading, Description: kicker}
	n.Add(gtx.Ops)
	access.Add(gtx, image.Rectangle{Max: dims.Size}, n, access.Actions{Tag: &c.title})
	if d.wantFocus && f.focus {
		gtx.Execute(key.FocusCmd{Tag: &c.title})
		d.wantFocus = false
	}
	call.Add(gtx.Ops)
	if gtx.Focused(&c.title) {
		focusRing(gtx, ps.palette.Text, dims.Size, dp(gtx, theme.RadiusEdit))
	}
	return dims
}

func (d *Dock) footer(gtx layout.Context, ps paints, f frame) layout.Dimensions {
	status := f.card.status(f.pending)
	return column(gtx, theme.Space3,
		d.statusLine(ps, f, status),
		d.retryButton(ps, f),
		func(gtx layout.Context) layout.Dimensions { return d.actionGrid(gtx, ps, f) },
		d.unsupportedLine(ps, f.unknown),
	)
}

func (d *Dock) statusLine(ps paints, f frame, status string) layout.Widget {
	if status == "" {
		return nil
	}
	ink, role := ps.palette.Dim, platform.RoleStatus
	if f.pending.view.Err != nil {
		ink, role = ps.palette.FailedText, platform.RoleError
	}
	return announced(ps, small(ink), role, status)
}

func announced(ps paints, st textStyle, role platform.Role, content string) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions { return drawText(gtx, ps.shaper, st, content) })
		defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
		platform.SemanticNode{Role: role, Name: content}.Add(gtx.Ops)
		call.Add(gtx.Ops)
		return dims
	}
}

func (d *Dock) retryButton(ps paints, f frame) layout.Widget {
	if f.pending.view.Err == nil {
		return nil
	}
	b := button{click: &f.card.retry, label: "Retry", variant: variantSecondary, height: theme.ControlHeight, size: theme.SizeLabel}
	return func(gtx layout.Context) layout.Dimensions { return b.layout(sized(gtx), ps) }
}

func (d *Dock) actionGrid(gtx layout.Context, ps paints, f frame) layout.Dimensions {
	buttons := make([]button, len(f.actions))
	question := f.card.details.kind == kindQuestion
	for i, a := range f.actions {
		buttons[i] = decision(&f.card.clicks[i], a.label, a.key, a.variant)
		buttons[i].reason = a.reason
		if question {
			buttons[i].height, buttons[i].size = theme.QuestionActionHeight, theme.SizeSmall
		}
		if question && a.variant == variantPoint {
			buttons[i].size = theme.SizeLabel
		}
	}
	columns := len(buttons)
	if f.card.details.approval() && columns == 4 {
		columns = 2
	}
	return grid(gtx, ps, columns, buttons)
}

func grid(gtx layout.Context, ps paints, columns int, buttons []button) layout.Dimensions {
	gap, width := dp(gtx, theme.Space2), gtx.Constraints.Max.X
	cell := (width - gap*(columns-1)) / columns
	for _, b := range buttons {
		if b.natural(gtx, ps) > cell {
			columns, cell = 1, width
			break
		}
	}
	y, row := 0, 0
	for i, b := range buttons {
		col := i % columns
		if col == 0 && i > 0 {
			y += row + gap
			row = 0
		}
		b.stretch = true
		cellGtx := gtx
		cellGtx.Constraints = layout.Constraints{Min: image.Pt(cell, 0), Max: image.Pt(cell, gtx.Constraints.Max.Y)}
		call, dims := giodraw.Record(cellGtx, func(gtx layout.Context) layout.Dimensions { return b.layout(gtx, ps) })
		giodraw.Place(gtx, image.Pt(col*(cell+gap), y), call)
		row = max(row, dims.Size.Y)
	}
	return layout.Dimensions{Size: image.Pt(width, y+row)}
}

func (d *Dock) layoutDetails(gtx layout.Context, ps paints, f frame) layout.Dimensions {
	items := d.detailItems(ps, f)
	d.items = len(items)
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions {
		return d.details.Layout(gtx, len(items), func(gtx layout.Context, i int) layout.Dimensions {
			return access.Scrollable(gtx, &d.details, i, items[i])
		})
	})
	d.viewport = dims.Size.Y
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	semantic.LabelOp(detailsLabel).Add(gtx.Ops)
	call.Add(gtx.Ops)
	return dims
}

func (d *Dock) layoutUnsupported(gtx layout.Context, env shell.Env, unknown []pending) layout.Dimensions {
	w := d.unsupportedLine(paints{shaper: env.Shaper, palette: env.Theme.Palette()}, unknown)
	if w == nil {
		return layout.Dimensions{}
	}
	return layout.UniformInset(unit.Dp(theme.Space2)).Layout(gtx, w)
}

func (d *Dock) unsupportedLine(ps paints, unknown []pending) layout.Widget {
	if len(unknown) == 0 {
		return nil
	}
	methods := make([]string, 0, len(unknown))
	for _, p := range unknown {
		methods = append(methods, p.view.Method+" on "+p.endpoint)
	}
	text := "This client cannot answer " + strings.Join(methods, ", ") + ". Update dalgona-gui to answer it."
	return announced(ps, small(ps.palette.WarningText), platform.RoleStatus, text)
}
