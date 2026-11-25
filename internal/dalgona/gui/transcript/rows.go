//go:build gui

package transcript

import (
	"strings"
	"unicode/utf8"

	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type synth uint8

const (
	synthNone synth = iota
	synthHistory
	synthOutcome
	synthReview
	synthPlan
	synthChildren
	synthField
)

const (
	synthTurn       = "\x00thread"
	itemHistory     = "\x00history"
	itemOutcome     = "\x00outcome"
	itemReview      = "\x00review"
	itemPlanPanel   = "\x00plan"
	itemChildren    = "\x00children"
	outcomeWorking  = "Working · new output appears here"
	outcomeStopped  = "Turn interrupted. Send a message to continue."
	historyIdle     = "Earlier turns are not loaded."
	historyLoading  = "Loading earlier turns"
	turnFailedLatch = ". Send your message again to retry, or start a new thread."
)

type childLink struct {
	key   state.ThreadKey
	title string
	state string
	open  widget.Clickable
}

type row struct {
	id             document.Identity
	synth          synth
	entry          Entry
	pres           Presentation
	sheet          sheet
	doc            *document.Document
	sourceEvicted  bool
	patchProjected bool
	turn           state.TurnView
	latest         bool
	children       []*childLink
	field          *viewField
	open           widget.Clickable
	retry          widget.Clickable
	toggle         widget.Clickable
	rewind         widget.Clickable
	fresh          widget.Clickable
	older          widget.Clickable
	sourceActions  sourceActions
	diffActions    diffActions
	folds          foldCache
	images         []*userImage
	highlights     map[document.Key]*codeHighlight
	used           uint64
}

func (r *row) detachFrameSources() {
	r.turn.Items = nil
	r.entry.Item.Tail, r.entry.Item.Changes = nil, nil
	if r.entry.Kind == KindGroup {
		for i := range r.entry.Members {
			it := &r.entry.Members[i].Item
			it.Tail, it.Changes = nil, nil
		}
	}
}

func identity(thread state.ThreadKey, turn, item string) document.Identity {
	if turn == "" {
		turn = synthTurn
	}
	return document.Identity{Endpoint: string(thread.Endpoint), Thread: thread.Thread, Turn: turn, Item: item}
}

func (p *pane) row(id document.Identity, kind synth) *row {
	r, ok := p.rows[id]
	if !ok {
		r = &row{id: id}
		p.rows[id] = r
	}
	r.synth, r.used = kind, p.frame
	return r
}

func (t *Transcript) desired(tv *state.ThreadView, p *pane, v *state.View) []*row {
	var out []*row
	key := tv.Key
	if !tv.HistoryComplete && len(tv.Turns) > 0 {
		out = append(out, p.row(identity(key, synthTurn, itemHistory), synthHistory))
	}
	last := len(tv.Turns) - 1
	for ti, turn := range tv.Turns {
		out = t.turnRows(out, p, key, turn, ti == last)
	}
	if len(tv.Children) > 0 {
		r := p.row(identity(key, synthTurn, itemChildren), synthChildren)
		r.children = childLinks(r.children, tv.Children, v)
		out = append(out, r)
	}
	for id, r := range p.rows {
		if r.used != p.frame {
			t.releaseImages(r)
			delete(p.rows, id)
		}
	}
	return out
}

func (t *Transcript) turnRows(out []*row, p *pane, key state.ThreadKey, turn state.TurnView, latest bool) []*row {
	entries := orderTurn(key, turn, t.factsOf)
	if t.level == LevelLow {
		entries = groupTools(entries)
	}
	for _, e := range entries {
		pres := present(e, t.level)
		if pres.Mode == ModeHidden {
			continue
		}
		r := p.row(identity(key, turn.ID, e.Item.Key.Item), synthNone)
		r.entry, r.pres, r.turn, r.latest = e, pres, turn, latest
		out = append(out, r)
	}
	for _, kind := range []synth{synthOutcome, synthReview, synthPlan} {
		if !turnShows(kind, turn, latest) {
			continue
		}
		r := p.row(identity(key, turn.ID, synthItems[kind]), kind)
		r.turn, r.latest = turn, latest
		out = append(out, r)
	}
	return out
}

var synthItems = map[synth]string{synthOutcome: itemOutcome, synthReview: itemReview, synthPlan: itemPlanPanel}

func turnShows(kind synth, turn state.TurnView, latest bool) bool {
	switch kind {
	case synthOutcome:
		return outcomeText(turn, latest) != ""
	case synthReview:
		return latest && turn.Review != nil
	case synthPlan:
		return latest && len(turn.Plan) > 0
	case synthNone, synthHistory, synthChildren, synthField:
	}
	return false
}

func outcomeText(turn state.TurnView, latest bool) string {
	switch turn.Status {
	case rpc.StatusInProgress:
		if turn.RetryError != nil {
			return "Retrying after an error: " + turn.RetryError.Message
		}
		if latest {
			return outcomeWorking
		}
	case rpc.StatusFailed:
		return failureText(turn, latest)
	case "interrupted":
		return outcomeStopped
	case rpc.StatusCompleted:
	}
	return ""
}

func failureText(turn state.TurnView, latest bool) string {
	message := "The turn failed"
	if turn.Error != nil {
		message += ": " + turn.Error.Message
	}
	if latest {
		return message + turnFailedLatch
	}
	if turn.Error == nil {
		return message + "."
	}
	return message
}

func childLinks(prev []*childLink, children []state.Child, v *state.View) []*childLink {
	known := make(map[state.ThreadKey]*childLink, len(prev))
	for _, c := range prev {
		known[c.key] = c
	}
	out := make([]*childLink, 0, len(children))
	for _, child := range children {
		link, ok := known[child.Thread]
		if !ok {
			link = &childLink{key: child.Thread}
		}
		link.title, link.state = child.Thread.Thread, "Ready"
		if summary, found := childSummary(v, child.Thread); found {
			link.title, link.state = threadTitle(summary), threadState(summary.Status)
		}
		out = append(out, link)
	}
	return out
}

func childSummary(v *state.View, key state.ThreadKey) (rpc.Thread, bool) {
	if tv, ok := v.Thread(key); ok && tv.Thread.ID != "" {
		return tv.Thread, true
	}
	return v.Summary(key)
}

func threadTitle(t rpc.Thread) string {
	switch {
	case t.Name != "":
		return t.Name
	case t.Preview != "":
		return t.Preview
	}
	return "Untitled thread"
}

func threadState(s rpc.ThreadStatus) string {
	for _, flag := range s.ActiveFlags {
		if s.Type == "active" && (flag == "waitingOnApproval" || flag == "waitingOnUserInput") {
			return "Waiting"
		}
	}
	return stateWord(s.Type)
}

func (t *Transcript) sheetFor(p *pane, r *row) sheet {
	switch r.synth {
	case synthHistory:
		return sheet{format: document.FormatNotice, source: historyIdle}
	case synthOutcome:
		return outcomeSheet(r)
	case synthReview:
		return reviewSheet(r.turn.Review)
	case synthPlan:
		return planSheet(r.turn.Plan)
	case synthChildren:
		return childrenSheet(r.children)
	case synthNone, synthField:
	}
	return t.entrySheet(p, r)
}

func (t *Transcript) entrySheet(p *pane, r *row) sheet {
	read := reader{loader: t.loader, thread: r.entry.Thread, item: r.entry.Item}
	if r.entry.Kind == KindGroup {
		return groupSheet(t, r.entry)
	}
	expanded, set := p.expanded[r.id]
	if !set {
		expanded = r.entry.Kind == KindThinking && r.entry.Live()
	}
	return buildSheet(sheetInput{
		entry: r.entry, pres: r.pres, read: read, expanded: expanded,
		diffSplit: r.diffActions.split, diffFull: r.diffActions.full,
	})
}

func groupSheet(t *Transcript, e Entry) sheet {
	summary := groupSummary(e.Members, func(m Entry) int {
		return fileCount(sheetInput{entry: m, read: reader{loader: t.loader, thread: m.Thread, item: m.Item}})
	})
	return sheet{format: document.FormatNotice, source: summary, full: true}
}

func outcomeSheet(r *row) sheet {
	text := outcomeText(r.turn, r.latest)
	role := roleDim
	if r.turn.Status == rpc.StatusFailed {
		role = roleFailed
	}
	return sheet{format: document.FormatNotice, source: text, roles: []lineRole{role}}
}

var verdictRoles = map[string]lineRole{"flagged": roleWarning, "error": roleFailed}

func reviewSheet(review *state.Review) sheet {
	role, ok := verdictRoles[review.Verdict]
	if !ok {
		role = roleDim
	}
	return sheet{format: document.FormatNotice, source: review.Summary, roles: repeatRole(review.Summary, role)}
}

func repeatRole(text string, role lineRole) []lineRole {
	roles := make([]lineRole, strings.Count(text, "\n")+1)
	for i := range roles {
		roles[i] = role
	}
	return roles
}

func planSheet(steps []rpc.TurnPlanStep) sheet {
	lines := make([]string, 0, len(steps))
	roles := make([]lineRole, 0, len(steps))
	for _, step := range steps {
		lines = append(lines, planWord(step.Status)+" · "+singleLine(step.Step))
		switch step.Status {
		case "inProgress":
			roles = append(roles, roleCurrent)
		case "completed":
			roles = append(roles, roleDim)
		default:
			roles = append(roles, roleText)
		}
	}
	return sheet{format: document.FormatNotice, source: strings.Join(lines, "\n"), roles: roles}
}

func singleLine(s string) string {
	return strings.ReplaceAll(s, "\n", " ")
}

func childrenSheet(children []*childLink) sheet {
	lines := make([]string, 0, len(children))
	for _, c := range children {
		lines = append(lines, singleLine(c.title)+" · "+c.state)
	}
	return sheet{format: document.FormatNotice, source: strings.Join(lines, "\n")}
}

func clean(s string) string {
	if utf8.ValidString(s) {
		return s
	}
	return strings.ToValidUTF8(s, "\uFFFD")
}

func (r *row) refresh(st *document.State) (fresh bool, changed bool) {
	s := r.sheet
	s.source = clean(s.source)
	if r.doc == nil || r.doc.Format() != s.format || r.patchProjected != s.patchProjection || (!r.doc.Open() && s.open) {
		doc, err := document.New(document.Header{Identity: r.id, Format: s.format, Open: s.open, PatchProjection: s.patchProjection}, s.source)
		if err != nil {
			return false, false
		}
		r.doc = doc
		r.patchProjected = s.patchProjection
		return true, true
	}
	return r.refreshSheet(st, s)
}

func (r *row) refreshSheet(st *document.State, s sheet) (fresh bool, changed bool) {
	if r.doc.Source() == s.source && r.doc.Open() == s.open {
		return false, false
	}
	if r.doc.Source() != s.source {
		next, change, err := r.advance(s)
		if err != nil {
			return false, false
		}
		r.doc = next
		if st.Publish(change, next) != nil {
			return true, true
		}
	}
	if r.doc.Open() && !s.open {
		return r.finish(st), true
	}
	return false, true
}

func (r *row) finish(st *document.State) bool {
	closed, change, err := r.doc.Close()
	if err != nil {
		return false
	}
	r.doc = closed
	return st.Publish(change, closed) != nil
}

func (r *row) advance(s sheet) (*document.Document, document.Change, error) {
	if r.doc.Open() && strings.HasPrefix(s.source, r.doc.Source()) {
		return r.doc.Append(s.source[len(r.doc.Source()):])
	}
	return r.doc.Replace(s.source)
}
