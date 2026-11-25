//go:build gui

package panel

import (
	"strings"
	"time"

	"gioui.org/layout"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	searchDelay  = 300 * time.Millisecond
	searchLimit  = 50
	openingText  = "Opening thread"
	noMentions   = "No conversation mentions this text."
	untitledText = "Untitled thread"
)

type laneSearch struct {
	call  pending
	hits  []rpc.ThreadSearchRow
	err   string
	done  bool
	retry Button
	items []Choice
}

type resumePanel struct {
	filter   field
	query    string
	changed  time.Time
	armed    bool
	searches map[client.EndpointID]*laneSearch
	rows     map[client.EndpointID][]Choice
	more     map[client.EndpointID]*Button
	asked    map[client.EndpointID]bool
	opening  state.ThreadKey
	openErr  string
}

func (r *resumePanel) open(shell.Route) {
	*r = resumePanel{
		filter:   newField("Filter threads", linePolicy),
		searches: map[client.EndpointID]*laneSearch{}, rows: map[client.EndpointID][]Choice{},
		more: map[client.EndpointID]*Button{}, asked: map[client.EndpointID]bool{},
	}
}

func (r *resumePanel) release(out *[]state.Intent) {
	for _, ls := range r.searches {
		ls.call.release(out)
	}
}

func (r *resumePanel) restart(out *[]state.Intent) {
	r.release(out)
	r.searches = map[client.EndpointID]*laneSearch{}
}

func connected(ep state.EndpointView) bool {
	return ep.Phase == client.PhaseConnected
}

func (r *resumePanel) search(s *scene, id client.EndpointID) {
	ls := r.searches[id]
	if ls == nil {
		ls = &laneSearch{}
		r.searches[id] = ls
	}
	ls.hits, ls.err, ls.done = nil, "", false
	ls.call.start(s, request{endpoint: id, method: "dalgona/thread/search", params: rpc.ThreadSearchParams{Query: r.query, Limit: searchLimit}})
}

func (r *resumePanel) update(gtx layout.Context, s *scene) {
	ev := r.filter.update(gtx)
	if ev.cancel {
		s.close()
		return
	}
	if text := r.filter.text(); text != r.query {
		r.query, r.changed, r.armed = text, gtx.Now, textinput.Searchable(text)
		r.restart(s.out)
	}
	r.page(gtx, s)
	if r.armed {
		r.fire(gtx, s)
	}
	r.settleSearches(gtx, s)
	r.choose(gtx, s)
	r.watchOpening(s)
}

func (r *resumePanel) fire(gtx layout.Context, s *scene) {
	due := r.changed.Add(searchDelay)
	if gtx.Now.Before(due) {
		gtx.Execute(op.InvalidateCmd{At: due})
		return
	}
	r.armed = false
	for _, ep := range s.env.View.Endpoints() {
		if connected(ep) {
			r.search(s, ep.ID)
		}
	}
}

func (r *resumePanel) settleSearches(gtx layout.Context, s *scene) {
	for id, ls := range r.searches {
		if ls.retry.Clicked(gtx) && !ls.call.running() {
			r.search(s, id)
		}
		v, done := ls.call.settle(s)
		if !done {
			continue
		}
		reply, err := decodeCall[rpc.ThreadSearchResponse](v)
		ls.done = true
		if err != nil {
			ls.err = err.Error()
			continue
		}
		ls.hits = reply.Data
	}
}

func (r *resumePanel) page(gtx layout.Context, s *scene) {
	for _, ep := range s.env.View.Endpoints() {
		lane := s.env.View.Lane(ep.ID)
		if b := r.more[ep.ID]; b != nil && b.Clicked(gtx) && !lane.Loading {
			s.env.Submit(state.LoadThreads{Endpoint: ep.ID})
		}
		if !lane.Loaded && !lane.Loading && lane.Err == nil && !r.asked[ep.ID] {
			r.asked[ep.ID] = true
			s.env.Submit(state.LoadThreads{Endpoint: ep.ID})
		}
	}
}

func (r *resumePanel) choose(gtx layout.Context, s *scene) {
	if r.opening.Thread != "" {
		return
	}
	for _, ep := range s.env.View.Endpoints() {
		rows := r.shown(s, ep)
		choices := r.rows[ep.ID]
		for i := range min(len(choices), len(rows)) {
			if choices[i].Clicked(gtx) {
				r.pick(s, state.ThreadKey{Endpoint: ep.ID, Thread: rows[i].ID})
			}
		}
	}
	for id, ls := range r.searches {
		for i := range min(len(ls.items), len(ls.hits)) {
			if ls.items[i].Clicked(gtx) {
				r.pick(s, state.ThreadKey{Endpoint: id, Thread: ls.hits[i].ThreadID})
			}
		}
	}
}

func (r *resumePanel) pick(s *scene, key state.ThreadKey) {
	r.opening, r.openErr = key, ""
	s.announce(platform.AnnounceProgress, openingText)
	if s.env.Navigate != nil {
		s.env.Navigate(key)
	}
}

func (r *resumePanel) watchOpening(s *scene) {
	if r.opening.Thread == "" {
		return
	}
	t, ok := s.env.View.Thread(r.opening)
	switch {
	case !ok || t.Opening:
	case t.Err != nil || t.Missing:
		r.openErr = "Unable to open the thread on " + endpointName(s.env.View, r.opening.Endpoint) + ": " + openFailure(t)
		r.opening = state.ThreadKey{}
	default:
		r.opening = state.ThreadKey{}
		s.close()
	}
}

func openFailure(t *state.ThreadView) string {
	if t.Err != nil {
		return t.Err.Error()
	}
	return "the thread no longer exists"
}

func threadName(t rpc.Thread) string {
	switch {
	case t.Name != "":
		return t.Name
	case t.Preview != "":
		return t.Preview
	}
	return untitledText
}

func (r *resumePanel) shown(s *scene, ep state.EndpointView) []rpc.Thread {
	rows := s.env.View.Lane(ep.ID).Rows
	needle := strings.ToLower(r.query)
	if strings.TrimSpace(needle) == "" {
		return rows
	}
	label := strings.ToLower(endpointName(s.env.View, ep.ID))
	var out []rpc.Thread
	for _, t := range rows {
		hay := strings.ToLower(threadName(t) + "\n" + t.Preview + "\n" + t.ID)
		if strings.Contains(hay, needle) || strings.Contains(label, needle) {
			out = append(out, t)
		}
	}
	return out
}

func (r *resumePanel) layout(gtx layout.Context, s *scene) layout.Dimensions {
	r.update(gtx, s)
	content := []layout.Widget{func(gtx layout.Context) layout.Dimensions { return r.filter.layout(gtx, s) }}
	if r.opening.Thread != "" {
		content = append(content, s.para(endpointName(s.env.View, r.opening.Endpoint)+" · "+openingText+"…"))
	}
	if r.openErr != "" {
		content = append(content, s.alert(r.openErr))
	}
	for _, ep := range s.env.View.Endpoints() {
		content = append(content, r.laneRows(s, ep)...)
	}
	if textinput.Searchable(r.query) {
		content = append(content, r.searchRows(s)...)
	}
	n, row := rows(content)
	return s.show(gtx, Content{Title: titles["resume"], Rows: n, Row: row})
}

func (r *resumePanel) laneRows(s *scene, ep state.EndpointView) []layout.Widget {
	lane := s.env.View.Lane(ep.ID)
	name := endpointName(s.env.View, ep.ID)
	out := []layout.Widget{s.legend(name + " · " + phaseWord(ep.Phase))}
	shown := r.shown(s, ep)
	for len(r.rows[ep.ID]) < len(shown) {
		r.rows[ep.ID] = append(r.rows[ep.ID], Choice{})
	}
	choices := r.rows[ep.ID]
	for i, t := range shown {
		o := Option{Label: threadName(t), Description: name, Disabled: r.opening.Thread != ""}
		out = append(out, func(gtx layout.Context) layout.Dimensions { return choices[i].Item(gtx, s.frame, s.env, o) })
	}
	return append(out, r.laneStatus(s, ep, lane, len(shown))...)
}

func (r *resumePanel) laneStatus(s *scene, ep state.EndpointView, lane state.LaneView, shown int) []layout.Widget {
	var out []layout.Widget
	switch {
	case lane.Err != nil:
		out = append(out, s.alert("Unable to list threads: "+lane.Err.Error()))
	case lane.Loading:
		out = append(out, s.note("Loading threads…"))
	case lane.Loaded && len(lane.Rows) == 0:
		out = append(out, s.note("No threads yet."))
	case lane.Loaded && shown == 0:
		out = append(out, s.note("No threads match this filter."))
	}
	if lane.More || lane.Err != nil {
		out = append(out, r.moreButton(s, ep.ID, lane))
	}
	return out
}

func (r *resumePanel) moreButton(s *scene, id client.EndpointID, lane state.LaneView) layout.Widget {
	b := r.more[id]
	if b == nil {
		b = &Button{}
		r.more[id] = b
	}
	label := "Load more threads"
	if lane.Err != nil {
		label = "Try again"
	}
	b.Busy = ""
	if lane.Loading {
		b.Busy = "Loading"
	}
	return func(gtx layout.Context) layout.Dimensions { return b.Layout(gtx, s.frame, s.env, label, Quiet) }
}

type searchTally struct {
	pending, failed bool
	hits            int
}

func (r *resumePanel) searchRows(s *scene) []layout.Widget {
	out := []layout.Widget{s.legend("Matches in conversations")}
	var tally searchTally
	for _, ep := range s.env.View.Endpoints() {
		out = append(out, r.endpointMatches(s, ep, &tally)...)
	}
	switch {
	case tally.pending:
		out = append(out, s.note("Searching conversations…"))
	case !tally.failed && tally.hits == 0:
		out = append(out, s.para(noMentions))
	}
	return out
}

func (r *resumePanel) endpointMatches(s *scene, ep state.EndpointView, tally *searchTally) []layout.Widget {
	ls := r.searches[ep.ID]
	name := endpointName(s.env.View, ep.ID)
	switch {
	case ls == nil && !connected(ep):
		return []layout.Widget{s.note(name + " is " + strings.ToLower(phaseWord(ep.Phase)) + "; its conversations are not searched.")}
	case ls == nil || !ls.done:
		tally.pending = true
		return nil
	case ls.err != "":
		tally.failed = true
		return r.failedRows(s, name, ls)
	}
	tally.hits += len(ls.hits)
	return r.hitRows(s, name, ls)
}

func (r *resumePanel) failedRows(s *scene, name string, ls *laneSearch) []layout.Widget {
	return []layout.Widget{
		s.alert(name + " · Search failed: " + ls.err + "."),
		func(gtx layout.Context) layout.Dimensions {
			return ls.retry.Layout(gtx, s.frame, s.env, "Try again", Quiet)
		},
	}
}

func (r *resumePanel) hitRows(s *scene, name string, ls *laneSearch) []layout.Widget {
	for len(ls.items) < len(ls.hits) {
		ls.items = append(ls.items, Choice{})
	}
	out := make([]layout.Widget, 0, len(ls.hits))
	for i, hit := range ls.hits {
		title := hit.Name
		if title == "" {
			title = hit.Preview
		}
		if title == "" {
			title = untitledText
		}
		o := Option{Label: title, Description: name + " · " + hit.Snippet, Disabled: r.opening.Thread != ""}
		out = append(out, func(gtx layout.Context) layout.Dimensions { return ls.items[i].Item(gtx, s.frame, s.env, o) })
	}
	return out
}
