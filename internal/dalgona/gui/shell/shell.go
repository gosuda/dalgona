//go:build gui

package shell

import (
	"image"
	"image/color"
	"time"

	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/text"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	transcriptLabel = "Transcript"
	dockLabel       = "Composer dock"
	skipLabel       = "Skip to transcript"
	backPrefix      = "Back to "
	helpRoute       = "help"
)

type Widget interface {
	Layout(gtx layout.Context, env Env) layout.Dimensions
}

type Panel interface {
	Widget
	Opened(r Route)
}

type Focusable interface {
	Widget
	FocusTag() event.Tag
}

type Request interface {
	Widget
	Showing() bool
	FocusReleased() bool
}

type Settler interface {
	Settle(env Env)
}

type Parts struct {
	Transcript Focusable
	Request    Request
	Composer   Focusable
	Panels     map[string]Panel
	Settlers   []Settler
}

type Config struct {
	Shaper      *text.Shaper
	Window      state.WindowID
	Submit      func(state.Intent)
	Announce    func(platform.Announcement)
	Clipboard   func() platform.ClipboardReader
	Wake        func()
	NewWindow   func()
	CloseWindow func()
	Parts       Parts
}

type Frame struct {
	Theme theme.Name
	View  *state.View
	Still bool
}

type frame struct {
	theme    theme.Name
	palette  theme.Palette
	view     *state.View
	still    bool
	lanes    []lane
	open     *state.ThreadView
	endpoint state.EndpointView
	env      Env
}

type Shell struct {
	shaper      *text.Shaper
	window      state.WindowID
	submitTo    func(state.Intent)
	announced   func(platform.Announcement)
	clipboard   func() platform.ClipboardReader
	wake        func()
	newWindow   func()
	closeWindow func()
	parts       Parts
	view        *state.View

	open      state.ThreadKey
	back      state.ThreadKey
	switching switching

	nav      navigator
	band     band
	notices  noticeStack
	panel    panelHost
	focus    focusTrack
	channels channels

	surface   layout.List
	dock      layout.List
	panelBody layout.List
	title     widget.Selectable
	backTo    widget.Clickable
	skip      widget.Clickable
	start     widget.Clickable
	archived  widget.Clickable
	menu      threadMenu
	apps      appMenu
	context   string
	connects  map[client.EndpointID]*widget.Clickable
	now       time.Time

	composerUsable bool
	composerHeight int

	close       closeFlow
	keepOpen    widget.Clickable
	closeAnyway widget.Clickable
}

func New(c Config) *Shell {
	return &Shell{
		shaper:      c.Shaper,
		window:      c.Window,
		submitTo:    c.Submit,
		announced:   c.Announce,
		clipboard:   c.Clipboard,
		wake:        c.Wake,
		newWindow:   c.NewWindow,
		closeWindow: c.CloseWindow,
		parts:       c.Parts,
		nav:         newNavigator(),
		notices:     newNoticeStack(),
		focus:       newFocusTrack(),
		surface:     layout.List{Axis: layout.Vertical},
		dock:        layout.List{Axis: layout.Vertical},
		panelBody:   layout.List{Axis: layout.Vertical},
		connects:    map[client.EndpointID]*widget.Clickable{},
	}
}

func (s *Shell) Open() state.ThreadKey { return s.open }

func (s *Shell) Show(key state.ThreadKey) { s.navigate(key, false) }

func (s *Shell) Adopt(key state.ThreadKey) { s.open = key }

func (s *Shell) submit(i state.Intent) {
	if s.submitTo != nil {
		s.submitTo(i)
	}
}

func (s *Shell) draftKey(k state.ThreadKey) state.DraftKey {
	return state.DraftKey{Endpoint: k.Endpoint, Window: s.window, Thread: k.Thread}
}

func (s *Shell) prepare(fr Frame) frame {
	s.view = fr.View
	s.settleSwitch()
	f := frame{theme: fr.Theme, palette: fr.Theme.Palette(), view: fr.View, still: fr.Still, lanes: projectLanes(fr.View)}
	if s.open.Thread != "" {
		f.open, _ = fr.View.Thread(s.open)
		f.endpoint, _ = endpointOf(fr.View, s.open.Endpoint)
	}
	f.env = Env{
		Theme: fr.Theme, Shaper: s.shaper, View: fr.View, Submit: s.submit, Window: s.window, Thread: s.open,
		Navigate:  func(k state.ThreadKey) { s.navigate(k, true) },
		Open:      func(r Route) { s.openPanel(r, nil) },
		Close:     s.closePanel,
		Announce:  s.announce,
		Clipboard: s.clipboard,
		Wake:      s.wake,
		Still:     fr.Still,
		Deciding:  s.requestShowing(),
	}
	s.composerUsable = usableComposer(f)
	return f
}

func (s *Shell) Layout(gtx layout.Context, fr Frame) bool {
	s.now = gtx.Now
	f := s.prepare(fr)
	s.focus.begin(gtx)
	for _, p := range s.parts.Settlers {
		p.Settle(f.env)
	}
	confirmed := s.update(gtx, f)
	f.env.Modal = s.modal()
	background := gtx
	if f.env.Modal {
		background = access.Hide(gtx)
	}
	giodraw.FillRect(gtx, f.palette.Canvas, image.Rectangle{Max: gtx.Constraints.Max})
	if gtx.Constraints.Max.X > dp(gtx, theme.ShellStackWidth) {
		s.layoutColumns(background, f)
	} else {
		s.layoutStacked(background, f)
	}
	s.overlay(gtx, f)
	s.focus.end(gtx, s.restoreTarget())
	s.flushAnnouncements()
	if due := s.channels.announcer.Due(); due.After(gtx.Now) {
		gtx.Execute(op.InvalidateCmd{At: due})
	}
	return confirmed
}

func (s *Shell) update(gtx layout.Context, f frame) bool {
	s.trackContext(gtx)
	s.prune(f.lanes)
	s.updateLanes(gtx, f.lanes)
	s.updateHeader(gtx, f)
	s.updateAppMenu(gtx)
	s.updateWorkspace(gtx, f)
	s.notices.update(gtx, s, f)
	confirmed := s.updateModals(gtx)
	s.updateKeys(gtx)
	if s.parts.Request != nil && s.parts.Request.FocusReleased() {
		s.focus.request(s.restoreTarget())
	}
	return confirmed
}

func (s *Shell) shellEscape(gtx layout.Context) bool {
	owned := s.close.asking() || s.switching.asking() || s.panel.showing(statusRoute) || s.menuOpen() && !s.panel.active()
	return owned && readEscape(gtx)
}

func (s *Shell) menuOpen() bool { return s.menu.open || s.apps.open }

func (s *Shell) updateModals(gtx layout.Context) bool {
	escape := s.shellEscape(gtx)
	closing, asking, status := s.close.asking(), s.switching.asking(), s.panel.showing(statusRoute)
	confirmed := s.updateClose(gtx, escape && closing)
	s.updateSwitch(gtx, escape && !closing)
	s.updateStatusPanel(gtx, escape && !closing && !asking)
	if escape && !closing && !asking && !status {
		s.dismissMenus()
	}
	return confirmed
}

func usableComposer(f frame) bool {
	return f.open != nil && f.endpoint.Phase == client.PhaseConnected
}

func (s *Shell) updateKeys(gtx layout.Context) {
	if s.modal() || s.composerFocused(gtx) {
		return
	}
	for {
		e, ok := gtx.Event(key.Filter{Name: "?", Optional: key.ModShift})
		if !ok {
			return
		}
		if k, isKey := e.(key.Event); isKey && k.State == key.Press {
			s.openPanel(Route{Panel: helpRoute, Thread: s.open, Context: s.helpContext(gtx)}, nil)
			return
		}
	}
}

func (s *Shell) helpContext(gtx layout.Context) string {
	switch {
	case s.requestShowing():
		return contextRequest
	case s.parts.Transcript != nil && gtx.Focused(s.parts.Transcript.FocusTag()), s.open.Thread != "":
		return contextTranscript
	}
	return ""
}

func (s *Shell) composerFocused(gtx layout.Context) bool {
	return s.parts.Composer != nil && gtx.Focused(s.parts.Composer.FocusTag())
}

func (s *Shell) modal() bool {
	return s.panel.active() || s.switching.asking() || s.close.asking()
}

func (s *Shell) updateWorkspace(gtx layout.Context, f frame) {
	if pressed(gtx, &s.skip, false) && s.parts.Transcript != nil {
		gtx.Execute(key.FocusCmd{Tag: s.parts.Transcript.FocusTag()})
	}
	if pressed(gtx, &s.start, s.panel.showing(newRoute)) {
		s.openPanel(Route{Panel: newRoute}, &s.start)
	}
	for id, c := range s.connects {
		if c.Clicked(gtx) {
			s.submit(state.Connect{Endpoint: id})
		}
	}
}

func (s *Shell) layoutColumns(gtx layout.Context, f frame) {
	size := gtx.Constraints.Max
	sidebar := min(dp(gtx, theme.SidebarMaxWidth), size.X)
	layout.Flex{Axis: layout.Horizontal}.Layout(gtx,
		layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			gtx.Constraints = layout.Exact(image.Pt(sidebar, size.Y))
			return s.navigation(gtx, f, size.Y)
		}),
		layout.Flexed(1, func(gtx layout.Context) layout.Dimensions {
			return s.workspaceColumn(gtx, f)
		}),
	)
}

func (s *Shell) workspaceColumn(gtx layout.Context, f frame) layout.Dimensions {
	size := gtx.Constraints.Max
	headerGtx := gtx
	headerGtx.Constraints = layout.Constraints{Min: image.Pt(size.X, 0), Max: size}
	headerCall, header := giodraw.Record(headerGtx, func(gtx layout.Context) layout.Dimensions { return s.header(gtx, f) })
	dockGtx := gtx
	dockGtx.Constraints = layout.Constraints{Max: image.Pt(size.X, max(0, size.Y-header.Size.Y-dp(gtx, theme.IdleDockClearance)))}
	dockCall, dock := giodraw.Record(dockGtx, func(gtx layout.Context) layout.Dimensions { return s.dockRegion(gtx, f, f.palette.Canvas) })
	headerCall.Add(gtx.Ops)
	surface := gtx
	surface.Constraints = layout.Exact(image.Pt(size.X, max(0, size.Y-header.Size.Y-dock.Size.Y)))
	func() {
		defer op.Offset(image.Pt(0, header.Size.Y)).Push(gtx.Ops).Pop()
		landmark(surface, transcriptLabel, f.palette.Canvas, func(gtx layout.Context) layout.Dimensions {
			return s.transcript(gtx, f)
		})
	}()
	giodraw.Place(gtx, image.Pt(0, size.Y-dock.Size.Y), dockCall)
	return layout.Dimensions{Size: size}
}

func (s *Shell) layoutStacked(gtx layout.Context, f frame) {
	size := gtx.Constraints.Max
	dockGtx := gtx
	dockGtx.Constraints = layout.Constraints{Max: image.Pt(size.X, max(0, size.Y-dp(gtx, theme.IdleDockClearance)))}
	m := op.Record(gtx.Ops)
	dockDims := s.dockRegion(dockGtx, f, f.palette.Surface)
	if dockDims.Size.Y > 0 {
		rule(gtx, f.palette.Line, dockDims.Size.X, 0)
	}
	dockCall := m.Stop()
	surfaceGtx := gtx
	surfaceGtx.Constraints = layout.Exact(image.Pt(size.X, size.Y-dockDims.Size.Y))
	s.surface.Layout(surfaceGtx, 3, func(gtx layout.Context, i int) layout.Dimensions {
		switch i {
		case 0:
			return s.navigation(gtx, f, dp(gtx, theme.ThreadListStackCap))
		case 1:
			return s.header(gtx, f)
		}
		return landmark(gtx, transcriptLabel, f.palette.Canvas, func(gtx layout.Context) layout.Dimensions {
			gtx.Constraints.Min.Y = 0
			return s.transcript(gtx, f)
		})
	})
	giodraw.Place(gtx, image.Pt(0, size.Y-dockDims.Size.Y), dockCall)
}

func (s *Shell) transcript(gtx layout.Context, f frame) layout.Dimensions {
	gtx.Constraints.Min.X = gtx.Constraints.Max.X
	call, _ := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions { return s.skipLink(gtx, f) })
	defer call.Add(gtx.Ops)
	if s.open.Thread != "" && s.parts.Transcript != nil {
		dims := s.parts.Transcript.Layout(gtx, f.env)
		dims.Size = gtx.Constraints.Constrain(dims.Size)
		return dims
	}
	dims := s.emptyWorkspace(gtx, f)
	dims.Size = gtx.Constraints.Constrain(dims.Size)
	return dims
}

func (s *Shell) skipLink(gtx layout.Context, f frame) layout.Dimensions {
	if s.parts.Transcript == nil {
		return layout.Dimensions{}
	}
	gtx.Constraints.Min = image.Point{}
	if gtx.Focused(&s.skip) {
		return s.control(gtx, f.palette, button{click: &s.skip, label: skipLabel, variant: secondary})
	}
	s.focus.drawn(gtx, &s.skip)
	dims := s.skip.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.Button.Add(gtx.Ops)
		semantic.LabelOp(skipLabel).Add(gtx.Ops)
		return layout.Dimensions{Size: image.Pt(dp(gtx, theme.TargetMin), 1)}
	})
	access.Add(gtx, image.Rectangle{Max: dims.Size}, platform.SemanticNode{Role: platform.RoleButton, Name: skipLabel, Action: "activate"}, access.Actions{Click: &s.skip})
	return dims
}

func (s *Shell) emptyWorkspace(gtx layout.Context, f frame) layout.Dimensions {
	p := f.palette
	kicker := textStyle{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeCaption, leading: theme.LeadingLabel, color: p.AccentText}
	heading := textStyle{face: platform.TitleFace, weight: theme.WeightTitle, size: theme.SizeTitle, leading: theme.LeadingCardTitle, color: p.Text}
	note := textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeBody, leading: theme.LeadingBody, color: p.Dim}
	inline := logical(theme.Space6)
	return layout.Inset{Top: logical(theme.Space8), Left: inline, Right: inline}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		gtx.Constraints.Max.X = min(gtx.Constraints.Max.X, dp(gtx, theme.TranscriptMaxWidth))
		gtx.Constraints.Min.X = min(gtx.Constraints.Min.X, gtx.Constraints.Max.X)
		return layout.Flex{Axis: layout.Vertical}.Layout(gtx,
			layout.Rigid(func(gtx layout.Context) layout.Dimensions { return drawText(gtx, s.shaper, kicker, "NO THREAD OPEN") }),
			layout.Rigid(layout.Spacer{Height: logical(theme.Space3)}.Layout),
			layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return drawText(gtx, s.shaper, heading, "Pick a thread or start a new one")
			}),
			layout.Rigid(layout.Spacer{Height: logical(theme.Space4)}.Layout),
			layout.Rigid(func(gtx layout.Context) layout.Dimensions { return drawText(gtx, s.shaper, note, emptyExplanation(f)) }),
			layout.Rigid(layout.Spacer{Height: logical(theme.Space6)}.Layout),
			layout.Rigid(func(gtx layout.Context) layout.Dimensions { return s.emptyActions(gtx, f) }),
		)
	})
}

func anyConnected(f frame) bool {
	for _, ep := range f.view.Endpoints() {
		if ep.Phase == client.PhaseConnected {
			return true
		}
	}
	return false
}

func emptyExplanation(f frame) string {
	if anyConnected(f) {
		return "Earlier threads appear in the list."
	}
	return "No endpoint has a live connection. Connect one to see its threads."
}

func (s *Shell) emptyActions(gtx layout.Context, f frame) layout.Dimensions {
	p := f.palette
	busy := s.panel.showing(newRoute)
	items := []layout.Widget{func(gtx layout.Context) layout.Dimensions {
		return s.control(gtx, p, button{click: &s.start, label: startLabel, variant: s.startVariant(), hue: f.theme.Point(""), busy: busy, note: busyNote(busy)})
	}}
	if !anyConnected(f) {
		items = append(items, s.connectButtons(f)...)
	}
	return wrapRow(gtx, theme.Space3, items...)
}

func (s *Shell) startVariant() variant {
	if s.modal() {
		return secondary
	}
	return pointed
}

func (s *Shell) connectButtons(f frame) []layout.Widget {
	var items []layout.Widget
	for _, ep := range f.view.Endpoints() {
		if !reconnectable(ep.Phase) && ep.Phase != client.PhaseIdle {
			continue
		}
		c, ok := s.connects[ep.ID]
		if !ok {
			c = new(widget.Clickable)
			s.connects[ep.ID] = c
		}
		label := connectLabel + " " + cmpOr(ep.Label, string(ep.ID))
		items = append(items, func(gtx layout.Context) layout.Dimensions {
			return s.control(gtx, f.palette, button{click: c, label: label, variant: secondary})
		})
	}
	return items
}

func (s *Shell) dockRegion(gtx layout.Context, f frame, ground color.NRGBA) layout.Dimensions {
	return landmark(gtx, dockLabel, ground, func(gtx layout.Context) layout.Dimensions {
		gtx.Constraints.Min.X = gtx.Constraints.Max.X
		return s.dock.Layout(gtx, 1, func(gtx layout.Context, _ int) layout.Dimensions { return s.dockContent(gtx, f) })
	})
}

func (s *Shell) dockContent(gtx layout.Context, f frame) layout.Dimensions {
	gtx.Constraints.Min = image.Point{X: gtx.Constraints.Max.X}
	width := gtx.Constraints.Max.X
	noticesCall, notices := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions { return s.notices.layout(gtx, s, f) })
	requestGtx := gtx
	requestGtx.Constraints.Max.Y = max(0, gtx.Constraints.Max.Y-notices.Size.Y-s.composerHeight)
	requestCall, request := giodraw.Record(requestGtx, func(gtx layout.Context) layout.Dimensions { return s.layoutPart(gtx, f, s.parts.Request) })
	composerCall, composer := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions { return s.layoutPart(gtx, f, s.parts.Composer) })
	s.composerHeight = composer.Size.Y
	noticesCall.Add(gtx.Ops)
	giodraw.Place(gtx, image.Pt(0, notices.Size.Y), requestCall)
	giodraw.Place(gtx, image.Pt(0, notices.Size.Y+request.Size.Y), composerCall)
	return layout.Dimensions{Size: image.Pt(width, notices.Size.Y+request.Size.Y+composer.Size.Y)}
}

func (s *Shell) layoutPart(gtx layout.Context, f frame, w Widget) layout.Dimensions {
	if w == nil {
		return layout.Dimensions{}
	}
	return w.Layout(gtx, f.env)
}

func (s *Shell) control(gtx layout.Context, p theme.Palette, b button) layout.Dimensions {
	s.focus.drawn(gtx, b.click)
	return drawButton(gtx, s.shaper, p, b)
}

func (s *Shell) mark(gtx layout.Context, tag *widget.Clickable, ink color.NRGBA, size image.Point) {
	s.focus.drawn(gtx, tag)
	if gtx.Focused(tag) {
		focusRing(gtx, ink, size, dp(gtx, theme.RadiusControl))
	}
}
