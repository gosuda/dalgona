//go:build gui

package shell

import (
	"image"
	"image/color"
	"slices"
	"time"

	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	navigationLabel = "Thread navigation"
	threadListLabel = "Conversation threads"
	startLabel      = "Start thread"
	loadMoreLabel   = "Load more threads"
	loadLabel       = "Load threads"
	retryLabel      = "Retry"
	connectLabel    = "Connect"
	unavailableText = "Threads unavailable"
	newRoute        = "new"
)

type laneRow struct {
	key     state.ThreadKey
	title   string
	preview string
	model   string
	updated int64
	tag     Tag
}

type lane struct {
	endpoint state.EndpointView
	list     state.LaneView
	rows     []laneRow
}

func endpointOf(v *state.View, id client.EndpointID) (state.EndpointView, bool) {
	eps := v.Endpoints()
	i := slices.IndexFunc(eps, func(e state.EndpointView) bool { return e.ID == id })
	if i < 0 {
		return state.EndpointView{ID: id, Label: string(id)}, false
	}
	return eps[i], true
}

func projectLanes(v *state.View) []lane {
	eps := v.Endpoints()
	lanes := make([]lane, 0, len(eps))
	for _, ep := range eps {
		l := lane{endpoint: ep, list: v.Lane(ep.ID)}
		listed := make(map[string]bool, len(l.list.Rows))
		for _, t := range l.list.Rows {
			listed[t.ID] = true
		}
		for _, key := range v.Threads() {
			if key.Endpoint == ep.ID && !listed[key.Thread] {
				tv, _ := v.Thread(key)
				l.rows = append(l.rows, rowOf(key, tv.Thread, tv, ep.Phase))
			}
		}
		for _, t := range l.list.Rows {
			key := state.ThreadKey{Endpoint: ep.ID, Thread: t.ID}
			tv, _ := v.Thread(key)
			l.rows = append(l.rows, rowOf(key, t, tv, ep.Phase))
		}
		lanes = append(lanes, l)
	}
	return lanes
}

func rowOf(key state.ThreadKey, t rpc.Thread, tv *state.ThreadView, phase client.Phase) laneRow {
	if tv != nil && tv.Thread.ID != "" {
		t = mergeSummary(t, tv.Thread)
	}
	return laneRow{key: key, title: threadTitle(t), preview: t.Preview, model: t.Model, updated: t.UpdatedAt, tag: rowTag(t, tv, phase)}
}

func mergeSummary(listed, live rpc.Thread) rpc.Thread {
	listed.Name = cmpOr(live.Name, listed.Name)
	listed.Preview = cmpOr(listed.Preview, live.Preview)
	listed.Model = cmpOr(live.Model, listed.Model)
	listed.UpdatedAt = max(listed.UpdatedAt, live.UpdatedAt)
	if live.Status.Type != "" {
		listed.Status = live.Status
	}
	return listed
}

func unavailable(p client.Phase) bool {
	return p == client.PhaseOffline || p == client.PhaseFailed || p == client.PhaseIncompatible
}

func reconnectable(p client.Phase) bool {
	return p == client.PhaseOffline || p == client.PhaseFailed
}

type laneControls struct {
	more    widget.Clickable
	load    widget.Clickable
	connect widget.Clickable
	start   widget.Clickable
}

type navigator struct {
	list    layout.List
	rows    map[state.ThreadKey]*widget.Clickable
	lanes   map[client.EndpointID]*laneControls
	start   widget.Clickable
	loaded  map[client.EndpointID]bool
	entries []layout.Widget
}

func newNavigator() navigator {
	return navigator{
		list:   layout.List{Axis: layout.Vertical},
		rows:   map[state.ThreadKey]*widget.Clickable{},
		lanes:  map[client.EndpointID]*laneControls{},
		loaded: map[client.EndpointID]bool{},
	}
}

func (n *navigator) row(key state.ThreadKey) *widget.Clickable {
	c, ok := n.rows[key]
	if !ok {
		c = new(widget.Clickable)
		n.rows[key] = c
	}
	return c
}

func (n *navigator) controls(id client.EndpointID) *laneControls {
	c, ok := n.lanes[id]
	if !ok {
		c = new(laneControls)
		n.lanes[id] = c
	}
	return c
}

func (s *Shell) updateLanes(gtx layout.Context, lanes []lane) {
	n := &s.nav
	if pressed(gtx, &n.start, false) {
		s.openPanel(Route{Panel: newRoute, Thread: state.ThreadKey{Endpoint: s.open.Endpoint}}, &n.start)
	}
	for _, l := range lanes {
		s.updateLane(gtx, l)
		for _, r := range l.rows {
			if n.row(r.key).Clicked(gtx) {
				s.navigate(r.key, false)
			}
		}
	}
}

func (s *Shell) updateLane(gtx layout.Context, l lane) {
	id := l.endpoint.ID
	s.pollLaneButtons(gtx, s.nav.controls(id), l)
	if l.endpoint.Kind == client.EndpointLocal && !s.nav.loaded[id] && !l.list.Loaded && !l.list.Loading {
		s.nav.loaded[id] = true
		s.submit(state.LoadThreads{Endpoint: id})
	}
}

func (s *Shell) pollLaneButtons(gtx layout.Context, c *laneControls, l lane) {
	live := l.endpoint.Phase == client.PhaseConnected
	if pressed(gtx, &c.more, l.list.Loading || !live) || pressed(gtx, &c.load, l.list.Loading) {
		s.submit(state.LoadThreads{Endpoint: l.endpoint.ID})
	}
	if pressed(gtx, &c.connect, false) {
		s.submit(state.Connect{Endpoint: l.endpoint.ID})
	}
	if pressed(gtx, &c.start, false) {
		s.openPanel(Route{Panel: newRoute, Thread: state.ThreadKey{Endpoint: l.endpoint.ID}}, &c.start)
	}
}

func (s *Shell) prune(lanes []lane) {
	keep := map[state.ThreadKey]bool{}
	ids := map[client.EndpointID]bool{}
	for _, l := range lanes {
		ids[l.endpoint.ID] = true
		for _, r := range l.rows {
			keep[r.key] = true
		}
	}
	for key := range s.nav.rows {
		if !keep[key] {
			delete(s.nav.rows, key)
		}
	}
	for id := range s.nav.lanes {
		if !ids[id] {
			delete(s.nav.lanes, id)
		}
	}
}

func (s *Shell) navigation(gtx layout.Context, f frame, listCap int) layout.Dimensions {
	n := platform.SemanticNode{ID: "navigation", Role: platform.RoleList, Name: navigationLabel}
	return access.Group(gtx, n, access.Actions{}, func(gtx layout.Context) layout.Dimensions {
		return s.navigationList(gtx, f, listCap)
	})
}

func (s *Shell) navigationList(gtx layout.Context, f frame, listCap int) layout.Dimensions {
	p := f.palette
	return landmark(gtx, navigationLabel, p.Tone, func(gtx layout.Context) layout.Dimensions {
		gtx.Constraints.Min.X = gtx.Constraints.Max.X
		gtx.Constraints.Max.Y = min(gtx.Constraints.Max.Y, listCap)
		inset := layout.Inset{Top: logical(theme.Space6), Bottom: logical(theme.Space6), Left: logical(theme.Space4), Right: logical(theme.Space4)}
		dims := inset.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			entries := s.sidebarEntries(f)
			return s.nav.list.Layout(gtx, len(entries), func(gtx layout.Context, i int) layout.Dimensions {
				gtx.Constraints.Min.X = gtx.Constraints.Max.X
				return access.Scrollable(gtx, &s.nav.list, i, entries[i])
			})
		})
		dims.Size.Y = max(dims.Size.Y, gtx.Constraints.Min.Y)
		giodraw.FillRect(gtx, p.Line, image.Rect(dims.Size.X-dp(gtx, theme.BorderWidth), 0, dims.Size.X, dims.Size.Y))
		return dims
	})
}

func (s *Shell) sidebarEntries(f frame) []layout.Widget {
	n := &s.nav
	n.entries = append(n.entries[:0], s.brand(f), s.threadHeading(f))
	for _, l := range f.lanes {
		n.entries = append(n.entries, s.laneHeader(f, l))
		n.entries = s.laneBody(n.entries, f, l)
	}
	n.entries = append(n.entries, s.archivedEntry(f))
	return n.entries
}

func (s *Shell) brand(f frame) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		p := f.palette
		eyebrow := textStyle{face: platform.TitleFace, weight: theme.WeightTitle, size: theme.SizeCaption, leading: theme.LeadingLabel, color: p.Dim}
		title := textStyle{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeTitle, leading: theme.LeadingCardTitle, color: p.Text, maxLines: 1}
		return layout.Inset{Bottom: logical(theme.Space8)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return layout.Flex{Axis: layout.Horizontal, Alignment: layout.End}.Layout(gtx,
				layout.Flexed(1, func(gtx layout.Context) layout.Dimensions {
					return layout.Flex{Axis: layout.Vertical}.Layout(gtx,
						layout.Rigid(func(gtx layout.Context) layout.Dimensions { return drawText(gtx, s.shaper, eyebrow, "AGENT WORKBENCH") }),
						layout.Rigid(func(gtx layout.Context) layout.Dimensions { return drawText(gtx, s.shaper, title, "dalgona") }),
					)
				}),
				layout.Rigid(func(gtx layout.Context) layout.Dimensions {
					return s.appMenuButton(gtx, f)
				}),
				layout.Rigid(func(gtx layout.Context) layout.Dimensions {
					return s.control(gtx, p, button{click: &s.nav.start, label: startLabel})
				}),
			)
		})
	}
}

func (s *Shell) threadHeading(f frame) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		p := f.palette
		overline := textStyle{face: platform.TitleFace, weight: theme.WeightTitle, size: theme.SizeCaption, leading: theme.LeadingLabel, color: p.Dim, maxLines: 1}
		dims := layout.Inset{Bottom: logical(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, s.shaper, overline, "THREADS")
		})
		rule(gtx, p.Line, dims.Size.X, dims.Size.Y-dp(gtx, theme.BorderWidth))
		return dims
	}
}

func phaseColor(p theme.Palette, phase client.Phase) color.NRGBA {
	switch phase {
	case client.PhaseOffline:
		return p.WarningText
	case client.PhaseIncompatible, client.PhaseFailed:
		return p.FailedText
	case client.PhaseIdle, client.PhaseConnecting, client.PhaseConnected, client.PhaseReconnecting:
		return p.Dim
	}
	return p.Dim
}

func (s *Shell) laneHeader(f frame, l lane) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		p := f.palette
		label := textStyle{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeSmall, leading: theme.LeadingCode, color: p.Text, maxLines: 1}
		phase := textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingCode, color: phaseColor(p, l.endpoint.Phase), maxLines: 1}
		name := cmpOr(l.endpoint.Label, string(l.endpoint.ID))
		word := phaseWord(l.endpoint.Phase)
		dims := labelled(gtx, name+", "+word, func(gtx layout.Context) layout.Dimensions {
			return layout.Inset{Top: logical(theme.Space4), Bottom: logical(theme.Space1)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
				return layout.Flex{Axis: layout.Horizontal, Alignment: layout.Baseline}.Layout(gtx,
					layout.Flexed(1, func(gtx layout.Context) layout.Dimensions { return paintText(gtx, s.shaper, label, name) }),
					layout.Rigid(func(gtx layout.Context) layout.Dimensions { return paintText(gtx, s.shaper, phase, word) }),
				)
			})
		})
		access.Add(gtx, image.Rectangle{Max: dims.Size}, platform.SemanticNode{ID: platform.NodeID(access.Path("endpoint", string(l.endpoint.ID))), Role: platform.RoleHeading, Level: 2, Name: name, Description: word}, access.Actions{})
		return dims
	}
}

func (s *Shell) laneBody(entries []layout.Widget, f frame, l lane) []layout.Widget {
	if unavailable(l.endpoint.Phase) {
		entries = append(entries, s.laneUnavailable(f, l))
	}
	if len(l.rows) > 0 {
		entries = append(entries, s.rowsBlock(f, l))
	}
	if w := s.laneFooter(f, l); w != nil {
		entries = append(entries, w)
	}
	return entries
}

func (s *Shell) laneUnavailable(f frame, l lane) layout.Widget {
	c := s.nav.controls(l.endpoint.ID)
	return func(gtx layout.Context) layout.Dimensions {
		p := f.palette
		lead := textStyle{face: platform.ProseFace, weight: theme.WeightControl, size: theme.SizeSmall, leading: theme.LeadingCode, color: p.Text}
		items := []layout.Widget{func(gtx layout.Context) layout.Dimensions { return drawText(gtx, s.shaper, lead, unavailableText) }}
		if reconnectable(l.endpoint.Phase) {
			items = append(items, func(gtx layout.Context) layout.Dimensions {
				return s.control(gtx, p, button{click: &c.connect, label: connectLabel, note: "Connect to " + cmpOr(l.endpoint.Label, string(l.endpoint.ID))})
			})
		}
		return wrapRow(gtx, theme.Space2, items...)
	}
}

func (s *Shell) laneFooter(f frame, l lane) layout.Widget {
	c := s.nav.controls(l.endpoint.ID)
	switch {
	case l.list.Err != nil:
		return s.laneMessage(f, "Could not load threads: "+l.list.Err.Error(), button{click: &c.load, label: retryLabel, busy: l.list.Loading})
	case l.list.More:
		return s.laneMessage(f, "", button{click: &c.more, label: loadMoreLabel, busy: laneBusy(l), note: busyNote(l.list.Loading)})
	case l.list.Loading && len(l.rows) == 0:
		return s.laneMessage(f, "Loading threads…", button{})
	}
	return s.laneEmptyFooter(f, l, c)
}

func laneBusy(l lane) bool {
	return l.list.Loading || l.endpoint.Phase != client.PhaseConnected
}

func (s *Shell) laneEmptyFooter(f frame, l lane, c *laneControls) layout.Widget {
	switch {
	case l.list.Loaded && len(l.rows) == 0 && l.endpoint.Phase == client.PhaseConnected:
		return s.laneMessage(f, "No threads yet", button{click: &c.start, label: startLabel, note: "on " + cmpOr(l.endpoint.Label, string(l.endpoint.ID))})
	case !l.list.Loaded && len(l.rows) == 0 && l.endpoint.Phase == client.PhaseIdle:
		return s.laneMessage(f, "", button{click: &c.load, label: loadLabel})
	}
	return nil
}

func busyNote(busy bool) string {
	if busy {
		return "Loading"
	}
	return ""
}

func (s *Shell) laneMessage(f frame, message string, action button) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		p := f.palette
		note := textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingCode, color: p.Dim}
		var items []layout.Widget
		if message != "" {
			items = append(items, func(gtx layout.Context) layout.Dimensions { return drawText(gtx, s.shaper, note, message) })
		}
		if action.click != nil {
			items = append(items, func(gtx layout.Context) layout.Dimensions { return s.control(gtx, p, action) })
		}
		return layout.Inset{Top: logical(theme.Space1), Bottom: logical(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return wrapRow(gtx, theme.Space2, items...)
		})
	}
}

func (s *Shell) rowsBlock(f frame, l lane) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		return landmark(gtx, threadListLabel+" on "+cmpOr(l.endpoint.Label, string(l.endpoint.ID)), color.NRGBA{}, func(gtx layout.Context) layout.Dimensions {
			children := make([]layout.FlexChild, len(l.rows))
			for i, r := range l.rows {
				children[i] = layout.Rigid(func(gtx layout.Context) layout.Dimensions { return s.threadRow(gtx, f, r) })
			}
			return layout.Flex{Axis: layout.Vertical}.Layout(gtx, children...)
		})
	}
}

func (s *Shell) threadRow(gtx layout.Context, f frame, r laneRow) layout.Dimensions {
	p := f.palette
	click := s.nav.row(r.key)
	selected := r.key == s.open
	return click.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.Button.Add(gtx.Ops)
		semantic.LabelOp(r.title).Add(gtx.Ops)
		semantic.SelectedOp(selected).Add(gtx.Ops)
		semantic.DescriptionOp(rowDescription(r)).Add(gtx.Ops)
		gtx.Constraints.Min.X = gtx.Constraints.Max.X
		dims := layout.UniformInset(logical(theme.Space3)).Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return s.rowContent(gtx, f, r, selected)
		})
		if selected {
			giodraw.FillRect(gtx, p.Text, image.Rect(0, 0, dp(gtx, theme.SelectionBarWidth), dims.Size.Y))
		}
		s.mark(gtx, click, p.Text, dims.Size)
		id := platform.NodeID(access.Path("thread", string(r.key.Endpoint), r.key.Thread))
		access.Add(gtx, image.Rectangle{Max: dims.Size}, platform.SemanticNode{ID: id, Role: platform.RoleButton, Name: r.title, Description: rowDescription(r), Action: "open", Selectable: true, Selected: selected}, access.Actions{Click: click})
		return dims
	})
}

func rowDescription(r laneRow) string {
	return r.tag.Label() + " · " + rowMeta(r)
}

func rowMeta(r laneRow) string {
	model := cmpOr(r.model, "Default model")
	if r.updated <= 0 {
		return model
	}
	return time.Unix(r.updated, 0).Format("Jan 2") + " · " + model
}

func (s *Shell) rowContent(gtx layout.Context, f frame, r laneRow, selected bool) layout.Dimensions {
	p := f.palette
	title := textStyle{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeSmall, leading: theme.LeadingCode, color: p.Text, maxLines: 2}
	if selected {
		title.weight = theme.WeightStrong
	}
	preview := textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingCode, color: p.Dim, maxLines: 2}
	meta := textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeCaption, leading: theme.LeadingLabel, color: p.Dim, maxLines: 1}
	children := []layout.FlexChild{
		layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return layout.Flex{Axis: layout.Horizontal, Alignment: layout.Middle}.Layout(gtx,
				layout.Rigid(func(gtx layout.Context) layout.Dimensions { return hueDot(gtx, f.theme, r.key.Thread) }),
				layout.Rigid(layout.Spacer{Width: logical(theme.Space2)}.Layout),
				layout.Flexed(1, func(gtx layout.Context) layout.Dimensions { return paintText(gtx, s.shaper, title, r.title) }),
			)
		}),
	}
	if r.preview != "" && r.preview != r.title {
		children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions { return paintText(gtx, s.shaper, preview, r.preview) }))
	}
	children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
		return row(gtx, theme.Space2,
			func(gtx layout.Context) layout.Dimensions {
				return drawTag(gtx, tagDraw{shaper: s.shaper, name: f.theme, hue: theme.Hue{}, tag: r.tag, still: f.still})
			},
			func(gtx layout.Context) layout.Dimensions { return paintText(gtx, s.shaper, meta, rowMeta(r)) },
		)
	}))
	return layout.Flex{Axis: layout.Vertical}.Layout(gtx, children...)
}

func hueDot(gtx layout.Context, name theme.Name, threadID string) layout.Dimensions {
	size := dp(gtx, theme.ThreadDotSize)
	if hue, ok := name.ThreadHue(threadID); ok {
		giodraw.FillRRect(gtx, hue.Fill, image.Rect(0, 0, size, size), size/2)
	}
	return layout.Dimensions{Size: image.Pt(size, size)}
}
