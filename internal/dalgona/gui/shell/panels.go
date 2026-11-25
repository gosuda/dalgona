//go:build gui

package shell

import (
	"image"

	"gioui.org/io/event"
	"gioui.org/io/pointer"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const closeLabel = "Close"

type panelHost struct {
	route  Route
	panel  Panel
	opener event.Tag
	open   bool
	scrim  int
	done   widget.Clickable
	folder widget.Selectable
}

func (h *panelHost) active() bool { return h.open }

func (h *panelHost) showing(name string) bool { return h.open && h.route.Panel == name }

func (s *Shell) openPanel(r Route, opener event.Tag) {
	if r.Panel == statusRoute {
		s.panel = panelHost{route: r, opener: opener, open: true, folder: s.panel.folder}
		s.focus.request(&s.panel.done)
		return
	}
	p, ok := s.parts.Panels[r.Panel]
	if !ok {
		s.notices.local("The "+r.Panel+" panel is not part of this build; update dalgona to use it", state.NoticeError)
		return
	}
	s.menu.open, s.apps.open = false, false
	s.panel = panelHost{route: r, panel: p, opener: opener, open: true, folder: s.panel.folder}
	p.Opened(r)
}

func (s *Shell) closePanel() {
	if !s.panel.open {
		return
	}
	opener := s.panel.opener
	s.panel.open, s.panel.panel = false, nil
	if s.focus.usable(opener) {
		s.focus.request(opener)
		return
	}
	s.focus.request(s.restoreTarget())
}

func (s *Shell) restoreTarget() event.Tag {
	switch {
	case s.open.Thread == "":
		return &s.start
	case s.parts.Composer != nil && s.composerUsable:
		return s.parts.Composer.FocusTag()
	case s.parts.Transcript != nil:
		return s.parts.Transcript.FocusTag()
	}
	return nil
}

func (s *Shell) updateStatusPanel(gtx layout.Context, escape bool) {
	if !s.panel.showing(statusRoute) {
		return
	}
	if s.panel.done.Clicked(gtx) || escape {
		s.closePanel()
	}
}

func (s *Shell) overlay(gtx layout.Context, f frame) {
	switch {
	case s.close.asking():
		s.block(gtx)
		s.closeCard(gtx, f)
	case s.switching.asking():
		s.block(gtx)
		s.switchCard(gtx, f)
	case s.panel.showing(statusRoute):
		s.block(gtx)
		s.statusCard(gtx, f)
	case s.panel.open:
		s.block(gtx)
		r := s.panel.route
		path := access.Path("panel", r.Panel, string(r.Thread.Endpoint), string(r.Thread.Thread), r.Turn)
		access.Scope(gtx, path, func(gtx layout.Context) layout.Dimensions { return s.panel.panel.Layout(gtx, f.env) })
	}
}

func (s *Shell) block(gtx layout.Context) {
	tag := &s.panel.scrim
	for {
		if _, ok := gtx.Event(pointer.Filter{Target: tag, Kinds: pointer.Press | pointer.Release | pointer.Move | pointer.Drag | pointer.Scroll, ScrollY: pointer.ScrollRange{Min: -1 << 20, Max: 1 << 20}, ScrollX: pointer.ScrollRange{Min: -1 << 20, Max: 1 << 20}}); !ok {
			break
		}
	}
	defer clip.Rect{Max: gtx.Constraints.Max}.Push(gtx.Ops).Pop()
	event.Op(gtx.Ops, tag)
}

func (s *Shell) statusCard(gtx layout.Context, f frame) {
	lines := []string{"No thread is open."}
	cwd := ""
	if f.open != nil {
		values := statusValues(f.view, f.open, s.queued(s.open))
		lines, cwd = values.details(), values.cwd
	}
	s.card(gtx, f, cardSpec{
		title:   "Status",
		lines:   lines,
		extra:   s.folderField(f, cwd),
		buttons: []button{{click: &s.panel.done, label: closeLabel, variant: pointed}},
	})
}

func (s *Shell) folderField(f frame, cwd string) layout.Widget {
	if cwd == "" {
		return nil
	}
	return func(gtx layout.Context) layout.Dimensions {
		p := f.palette
		if s.panel.folder.Text() != cwd {
			s.panel.folder.SetText(cwd)
		}
		st := textStyle{face: platform.CodeFace, weight: theme.WeightText}
		return labelled(gtx, "Working directory "+cwd, func(gtx layout.Context) layout.Dimensions {
			return s.panel.folder.Layout(gtx, s.shaper, font(st), unit.Sp(theme.SizeSmall), giodraw.Material(gtx, p.Text), giodraw.Material(gtx, p.Tone))
		})
	}
}

type cardSpec struct {
	title   string
	lines   []string
	extra   layout.Widget
	buttons []button
}

func (s *Shell) card(gtx layout.Context, f frame, spec cardSpec) {
	p := f.palette
	size := gtx.Constraints.Max
	clearance := min(dp(gtx, theme.Space8), size.X/8, size.Y/8)
	width := min(dp(gtx, theme.ModalMaxWidth), size.X-2*clearance)
	pad := dp(gtx, theme.Space5)
	inner := gtx
	inner.Constraints = layout.Constraints{
		Min: image.Pt(max(0, width-2*pad), 0),
		Max: image.Pt(max(0, width-2*pad), max(0, size.Y-2*clearance-2*pad)),
	}
	title, titleDims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions {
		st := textStyle{face: platform.TitleFace, weight: theme.WeightTitle, size: theme.SizeHeading, leading: theme.LeadingQuestion, color: p.Text}
		return labelled(gtx, spec.title, func(gtx layout.Context) layout.Dimensions { return paintText(gtx, s.shaper, st, spec.title) })
	})
	footer, footerDims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions { return s.cardButtons(gtx, p, spec.buttons) })
	gap := dp(gtx, theme.Space4)
	bodyGtx := inner
	bodyGtx.Constraints.Max.Y = max(0, inner.Constraints.Max.Y-titleDims.Size.Y-footerDims.Size.Y-2*gap)
	body, bodyDims := giodraw.Record(bodyGtx, func(gtx layout.Context) layout.Dimensions { return s.cardBody(gtx, p, spec) })
	height := 2*pad + titleDims.Size.Y + bodyDims.Size.Y + footerDims.Size.Y + 2*gap
	origin := image.Pt((size.X-width)/2, max(clearance, (size.Y-height)/2))
	defer op.Offset(origin).Push(gtx.Ops).Pop()
	frameRect := image.Rect(0, 0, width, height)
	giodraw.FillRRect(gtx, p.Surface, frameRect, dp(gtx, theme.RadiusCard))
	giodraw.StrokeRRect(gtx, p.Line, frameRect, dp(gtx, theme.RadiusCard), dp(gtx, theme.BorderWidth))
	giodraw.Place(gtx, image.Pt(pad, pad), title)
	giodraw.Place(gtx, image.Pt(pad, pad+titleDims.Size.Y+gap), body)
	giodraw.Place(gtx, image.Pt(pad, pad+titleDims.Size.Y+gap+bodyDims.Size.Y+gap), footer)
}

func (s *Shell) cardBody(gtx layout.Context, p theme.Palette, spec cardSpec) layout.Dimensions {
	prose := textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeBody, leading: theme.LeadingBody, color: p.Text}
	count := len(spec.lines)
	if spec.extra != nil {
		count++
	}
	return s.panelBody.Layout(gtx, count, func(gtx layout.Context, i int) layout.Dimensions {
		return layout.Inset{Bottom: logical(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			if i == len(spec.lines) {
				return spec.extra(gtx)
			}
			return drawText(gtx, s.shaper, prose, spec.lines[i])
		})
	})
}

func (s *Shell) cardButtons(gtx layout.Context, p theme.Palette, buttons []button) layout.Dimensions {
	gap := dp(gtx, theme.Space3)
	room := gtx.Constraints.Max.X
	child := gtx
	child.Constraints.Min = image.Point{}
	calls := make([]op.CallOp, len(buttons))
	sizes := make([]image.Point, len(buttons))
	total, tallest := 0, 0
	for i, b := range buttons {
		call, dims := giodraw.Record(child, func(gtx layout.Context) layout.Dimensions { return s.control(gtx, p, b) })
		calls[i], sizes[i] = call, dims.Size
		total += dims.Size.X
		tallest = max(tallest, dims.Size.Y)
	}
	total += gap * max(0, len(buttons)-1)
	if total <= room {
		x := room - total
		for i := range calls {
			giodraw.Place(gtx, image.Pt(x, 0), calls[i])
			x += sizes[i].X + gap
		}
		return layout.Dimensions{Size: image.Pt(room, tallest)}
	}
	y := 0
	for i := len(calls) - 1; i >= 0; i-- {
		giodraw.Place(gtx, image.Pt(0, y), calls[i])
		y += sizes[i].Y + gap
	}
	return layout.Dimensions{Size: image.Pt(room, max(0, y-gap))}
}
