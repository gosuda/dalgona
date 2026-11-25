//go:build gui

package shell

import (
	"image"

	"gioui.org/io/key"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	menuLabel     = "Thread actions"
	renameRoute   = "rename"
	archiveRoute  = "archive"
	archivedRoute = "archived"
)

var menuEntries = [...]struct{ label, route string }{
	{"Rename", renameRoute},
	{"Archive", archiveRoute},
	{"Archived threads", archivedRoute},
}

type threadMenu struct {
	toggle widget.Clickable
	items  [len(menuEntries)]widget.Clickable
	open   bool
}

func (s *Shell) openTitle(f frame) string {
	if f.open != nil && f.open.Thread.ID != "" {
		return threadTitle(f.open.Thread)
	}
	for _, l := range f.lanes {
		for _, r := range l.rows {
			if r.key == s.open {
				return r.title
			}
		}
	}
	return s.open.Thread
}

func (s *Shell) headerTag(f frame) (Tag, bool) {
	if f.open == nil || f.open.Opening {
		return Tag{}, false
	}
	t := threadTag(f.open, f.endpoint.Phase)
	t.Point = t.Word == Working && !s.modal() && !s.requestShowing()
	return t, true
}

func (s *Shell) requestShowing() bool {
	return s.parts.Request != nil && s.parts.Request.Showing()
}

func (s *Shell) WindowTitle(fr Frame) string {
	if s.open.Thread == "" {
		return "dalgona"
	}
	f := s.prepare(fr)
	t, ok := s.headerTag(f)
	if !ok {
		return "dalgona · " + s.openTitle(f)
	}
	return "dalgona · " + s.openTitle(f) + " · " + t.Name()
}

func (s *Shell) updateHeader(gtx layout.Context, f frame) {
	if pressed(gtx, &s.backTo, false) && s.back.Thread != "" {
		s.navigate(s.back, false)
	}
	if s.menu.toggle.Clicked(gtx) {
		s.menu.open, s.apps.open = !s.menu.open, false
		if s.menu.open {
			s.focus.request(&s.menu.items[0])
		}
	}
	for i := range s.menu.items {
		if s.menu.items[i].Clicked(gtx) {
			s.menu.open = false
			s.openPanel(Route{Panel: menuEntries[i].route, Thread: s.open}, &s.menu.toggle)
		}
	}
	if s.open.Thread != "" && f.open != nil {
		queued := len(f.view.FollowUps(s.draftKey(s.open)))
		s.band.update(gtx, statusValues(f.view, f.open, queued), func(route string, opener *widget.Clickable) {
			s.openPanel(Route{Panel: route, Thread: s.open}, opener)
		})
	}
	s.updateArchived(gtx)
}

func readEscape(gtx layout.Context) bool {
	pressed := false
	for {
		e, ok := gtx.Event(key.Filter{Name: key.NameEscape})
		if !ok {
			return pressed
		}
		if k, isKey := e.(key.Event); isKey && k.State == key.Press {
			pressed = true
		}
	}
}

func (s *Shell) header(gtx layout.Context, f frame) layout.Dimensions {
	if s.open.Thread == "" {
		return layout.Dimensions{}
	}
	compact := gtx.Constraints.Max.X <= dp(gtx, theme.StatusCompactWidth)
	block, inline := logical(theme.Space4), logical(theme.Space6)
	if compact {
		block, inline = logical(theme.Space3), logical(theme.Space4)
	}
	return landmark(gtx, "", f.palette.Surface, func(gtx layout.Context) layout.Dimensions {
		gtx.Constraints.Min.X = gtx.Constraints.Max.X
		dims := layout.Inset{Top: block, Bottom: block, Left: inline, Right: inline}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return s.headerContent(gtx, f, compact)
		})
		rule(gtx, f.palette.Line, dims.Size.X, dims.Size.Y-dp(gtx, theme.BorderWidth))
		return dims
	})
}

func (s *Shell) headerContent(gtx layout.Context, f frame, compact bool) layout.Dimensions {
	band := func(gtx layout.Context) layout.Dimensions { return s.statusBand(gtx, f, compact) }
	title := func(gtx layout.Context) layout.Dimensions { return s.titleColumn(gtx, f) }
	if compact {
		return layout.Flex{Axis: layout.Vertical}.Layout(gtx,
			layout.Rigid(title),
			layout.Rigid(layout.Spacer{Height: logical(theme.Space2)}.Layout),
			layout.Rigid(band),
		)
	}
	width := gtx.Constraints.Max.X
	gap := dp(gtx, theme.Space6)
	bandWidth := max(dp(gtx, theme.BandMinWidth), (width-gap-dp(gtx, theme.TitleBasis))/2)
	titleWidth := max(0, width-gap-bandWidth)
	return layout.Flex{Axis: layout.Horizontal, Alignment: layout.Start}.Layout(gtx,
		layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			gtx.Constraints = layout.Constraints{Min: image.Pt(titleWidth, 0), Max: image.Pt(titleWidth, gtx.Constraints.Max.Y)}
			return title(gtx)
		}),
		layout.Rigid(layout.Spacer{Width: logical(theme.Space6)}.Layout),
		layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			gtx.Constraints = layout.Constraints{Min: image.Pt(bandWidth, 0), Max: image.Pt(bandWidth, gtx.Constraints.Max.Y)}
			return band(gtx)
		}),
	)
}

func (s *Shell) statusBand(gtx layout.Context, f frame, compact bool) layout.Dimensions {
	if f.open == nil || f.open.Opening {
		return layout.Dimensions{}
	}
	queued := len(f.view.FollowUps(s.draftKey(s.open)))
	gtx.Constraints.Min.X = 0
	return s.band.layout(gtx, s.shaper, f.theme, statusValues(f.view, f.open, queued), compact)
}

func (s *Shell) titleColumn(gtx layout.Context, f frame) layout.Dimensions {
	children := []layout.FlexChild{}
	if s.back.Thread != "" {
		children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return s.control(gtx, f.palette, button{click: &s.backTo, label: backPrefix + s.backTitle(f)})
		}))
	}
	children = append(children,
		layout.Rigid(func(gtx layout.Context) layout.Dimensions { return s.titleText(gtx, f) }),
		layout.Rigid(layout.Spacer{Height: logical(theme.Space2)}.Layout),
		layout.Rigid(func(gtx layout.Context) layout.Dimensions { return s.stateLine(gtx, f) }),
	)
	return layout.Flex{Axis: layout.Vertical}.Layout(gtx, children...)
}

func (s *Shell) backTitle(f frame) string {
	if tv, ok := f.view.Thread(s.back); ok && tv.Thread.ID != "" {
		return threadTitle(tv.Thread)
	}
	return "parent thread"
}

func (s *Shell) titleText(gtx layout.Context, f frame) layout.Dimensions {
	p := f.palette
	full := s.openTitle(f)
	if s.title.Text() != full {
		s.title.SetText(full)
	}
	s.title.MaxLines = 2
	s.title.LineHeightScale = theme.LeadingQuestion
	st := textStyle{face: platform.TitleFace, weight: theme.WeightControl}
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions {
		return s.title.Layout(gtx, s.shaper, font(st), unit.Sp(theme.SizeHeading), giodraw.Material(gtx, p.Text), giodraw.Material(gtx, p.Tone))
	})
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	semantic.LabelOp(full).Add(gtx.Ops)
	call.Add(gtx.Ops)
	return dims
}

func (s *Shell) stateLine(gtx layout.Context, f frame) layout.Dimensions {
	p := f.palette
	items := []layout.Widget{}
	if t, ok := s.headerTag(f); ok {
		items = append(items, func(gtx layout.Context) layout.Dimensions {
			return drawTag(gtx, tagDraw{shaper: s.shaper, name: f.theme, hue: f.theme.Point(s.open.Thread), tag: t, still: f.still})
		})
	}
	if line := endpointLine(f); line != "" {
		st := textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingCode, color: phaseColor(p, f.endpoint.Phase), maxLines: 1}
		items = append(items, func(gtx layout.Context) layout.Dimensions { return drawText(gtx, s.shaper, st, line) })
	}
	items = append(items, func(gtx layout.Context) layout.Dimensions { return s.menuButton(gtx, f) })
	return wrapRow(gtx, theme.Space3, items...)
}

func endpointLine(f frame) string {
	label := cmpOr(f.endpoint.Label, string(f.endpoint.ID))
	if f.endpoint.Phase != client.PhaseConnected {
		return label + " · " + phaseWord(f.endpoint.Phase)
	}
	if len(f.view.Endpoints()) > 1 {
		return label
	}
	return ""
}

func (s *Shell) menuButton(gtx layout.Context, f frame) layout.Dimensions {
	toggle := button{click: &s.menu.toggle, label: menuLabel, id: "thread-commands", expandable: true, expanded: s.menu.open}
	if !s.menu.open {
		return s.dropdown(gtx, f, toggle, nil)
	}
	items := make([]button, len(menuEntries))
	for i, e := range menuEntries {
		items[i] = button{click: &s.menu.items[i], label: e.label, role: platform.RoleMenuItem}
	}
	return s.dropdown(gtx, f, toggle, items)
}

func (s *Shell) dropdown(gtx layout.Context, f frame, toggle button, items []button) layout.Dimensions {
	dims := s.control(gtx, f.palette, toggle)
	if len(items) == 0 {
		return dims
	}
	m := op.Record(gtx.Ops)
	func() {
		defer op.Offset(image.Pt(0, dims.Size.Y+dp(gtx, theme.Space1))).Push(gtx.Ops).Pop()
		s.menuCard(gtx, f, items)
	}()
	op.Defer(gtx.Ops, m.Stop())
	return dims
}

func (s *Shell) menuCard(gtx layout.Context, f frame, items []button) layout.Dimensions {
	p := f.palette
	pad := dp(gtx, theme.Space2)
	inner := gtx
	inner.Constraints.Min = image.Point{}
	call, dims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions {
		children := make([]layout.FlexChild, len(items))
		for i, b := range items {
			children[i] = layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return s.control(gtx, p, b)
			})
		}
		return layout.Flex{Axis: layout.Vertical}.Layout(gtx, children...)
	})
	size := dims.Size.Add(image.Pt(2*pad, 2*pad))
	card := image.Rectangle{Max: size}
	giodraw.FillRRect(gtx, p.Surface, card, dp(gtx, theme.RadiusCard))
	giodraw.StrokeRRect(gtx, p.Line, card, dp(gtx, theme.RadiusCard), dp(gtx, theme.BorderWidth))
	giodraw.Place(gtx, image.Pt(pad, pad), call)
	return layout.Dimensions{Size: size}
}

func (s *Shell) archivedEntry(f frame) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		return layout.Inset{Top: logical(theme.Space4)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return s.control(gtx, f.palette, button{click: &s.archived, label: "Archived threads"})
		})
	}
}

func (s *Shell) updateArchived(gtx layout.Context) {
	if pressed(gtx, &s.archived, false) {
		s.openPanel(Route{Panel: archivedRoute, Thread: state.ThreadKey{}}, &s.archived)
	}
}
