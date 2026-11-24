//go:build gui

package composer

import (
	"image"

	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op/clip"
	"gioui.org/op/paint"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

type listRow struct {
	primary   string
	secondary string
	reason    string
}

type choiceList struct {
	clicks []widget.Clickable
	list   layout.List
	active int
}

func (c *choiceList) clicked(gtx layout.Context) (int, bool) {
	for i := range c.clicks {
		if c.clicks[i].Clicked(gtx) {
			return i, true
		}
	}
	return 0, false
}

func (c *choiceList) layout(gtx layout.Context, look dockLook, label string, rows []listRow, active int) layout.Dimensions {
	if len(c.clicks) != len(rows) {
		c.clicks = make([]widget.Clickable, len(rows))
	}
	c.list.Axis = layout.Vertical
	if active != c.active {
		c.active = active
		c.list.ScrollTo(active)
	}
	gtx.Constraints.Min = image.Pt(gtx.Constraints.Max.X, 0)
	gtx.Constraints.Max.Y = max(dp(gtx, theme.ControlHeight), gtx.Constraints.Max.Y/2)
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions {
		return c.list.Layout(gtx, len(rows), func(gtx layout.Context, i int) layout.Dimensions {
			return access.Scope(gtx, access.Path("suggestions", label), func(gtx layout.Context) layout.Dimensions { return c.row(gtx, look, rows[i], i) })
		})
	})
	bounds := image.Rectangle{Max: dims.Size}
	radius := dp(gtx, theme.RadiusCard)
	defer clip.UniformRRect(bounds, radius).Push(gtx.Ops).Pop()
	semantic.LabelOp(label).Add(gtx.Ops)
	paint.Fill(gtx.Ops, look.palette.Surface)
	call.Add(gtx.Ops)
	giodraw.StrokeRRect(gtx, look.palette.Line, bounds, radius, dp(gtx, theme.BorderWidth))
	return dims
}

func (c *choiceList) row(gtx layout.Context, look dockLook, r listRow, i int) layout.Dimensions {
	active, p := i == c.active, look.palette
	return c.clicks[i].Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.Button.Add(gtx.Ops)
		semantic.LabelOp(r.primary).Add(gtx.Ops)
		semantic.DescriptionOp(rowDescription(r)).Add(gtx.Ops)
		semantic.SelectedOp(active).Add(gtx.Ops)
		semantic.EnabledOp(r.reason == "").Add(gtx.Ops)
		inset := layout.Inset{Top: unit.Dp(theme.Space2), Bottom: unit.Dp(theme.Space2), Left: unit.Dp(theme.Space3), Right: unit.Dp(theme.Space3)}
		call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions {
			return inset.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
				return flow(gtx, theme.Space3,
					func(gtx layout.Context) layout.Dimensions {
						st := small(p.Text)
						st.face, st.weight = platform.CodeFace, theme.WeightStrong
						return drawText(gtx, look.shaper, st, r.primary)
					},
					func(gtx layout.Context) layout.Dimensions {
						return drawText(gtx, look.shaper, small(p.Dim), rowDescription(r))
					},
				)
			})
		})
		size := image.Pt(gtx.Constraints.Max.X, dims.Size.Y)
		if active {
			paint.FillShape(gtx.Ops, p.Tone, clip.Rect{Max: size}.Op())
			paint.FillShape(gtx.Ops, p.Text, clip.Rect{Max: image.Pt(dp(gtx, theme.SelectionBarWidth), size.Y)}.Op())
		}
		call.Add(gtx.Ops)
		c.registerChoice(gtx, size, r, i)
		return layout.Dimensions{Size: size}
	})
}

func (c *choiceList) registerChoice(gtx layout.Context, size image.Point, r listRow, i int) {
	n := platform.SemanticNode{ID: platform.NodeID(access.Path(r.primary, r.secondary)), Role: platform.RoleButton, Name: r.primary, Description: r.secondary, Reason: r.reason, Disabled: r.reason != "", Selectable: true, Selected: i == c.active, Action: "select"}
	access.Add(gtx, image.Rectangle{Max: size}, n, access.Actions{Click: &c.clicks[i], Scroll: func() { c.list.ScrollTo(i) }})
}

func rowDescription(r listRow) string {
	if r.reason == "" {
		return r.secondary
	}
	return r.secondary + " · " + r.reason
}
