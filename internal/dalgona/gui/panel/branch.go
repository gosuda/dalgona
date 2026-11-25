//go:build gui

package panel

import (
	"slices"

	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	noThreadText  = "Open a thread first. This panel acts on the thread on screen."
	chooseTurn    = "Choose a turn first."
	observeOnly   = "Closing stops watching; the daemon finishes the operation."
	unknownResult = "The connection dropped before the daemon answered. The outcome is checked when the endpoint reconnects."
)

func targetLine(s *scene, thread state.ThreadKey) layout.Widget {
	t, _ := s.env.View.Thread(thread)
	return s.note("Thread: " + threadTitle(t) + " on " + endpointName(s.env.View, thread.Endpoint))
}

func noThread(gtx layout.Context, s *scene, panel string) layout.Dimensions {
	n, row := rows([]layout.Widget{s.para(noThreadText)})
	return s.show(gtx, Content{Title: titles[panel], Rows: n, Row: row})
}

type operation struct {
	call   pending
	run    Button
	err    string
	failed string
}

func (o *operation) reset() {
	*o = operation{}
}

func (o *operation) start(s *scene, r request) {
	o.err = ""
	r.operation = true
	o.call.start(s, r)
}

func (o *operation) settle(s *scene) (state.CallView, bool) {
	v, done := o.call.settle(s)
	if done && v.State != state.CallSucceeded {
		o.err = o.failed + failure(v)
		s.announce(platform.AnnounceFailure, o.err)
		return v, false
	}
	return v, done
}

func (o *operation) status(s *scene, busy string) []layout.Widget {
	var out []layout.Widget
	if o.call.running() {
		out = append(out, s.note(busy+". "+observeOnly))
	}
	if o.call.unknown(s) {
		out = append(out, s.alert(unknownResult))
	}
	if o.err != "" {
		out = append(out, s.alert(o.err))
	}
	return out
}

func (o *operation) button(s *scene, label, busy string, kind Kind) layout.Widget {
	o.run.Busy = ""
	if o.call.running() {
		o.run.Busy = busy
	}
	return func(gtx layout.Context) layout.Dimensions { return o.run.Layout(gtx, s.frame, s.env, label, kind) }
}

type compactPanel struct {
	thread state.ThreadKey
	op     operation
}

func (c *compactPanel) open(r shell.Route) {
	c.thread = r.Thread
	c.op.reset()
}

func (c *compactPanel) release(out *[]state.Intent) { c.op.call.release(out) }

func (c *compactPanel) layout(gtx layout.Context, s *scene) layout.Dimensions {
	if c.thread.Thread == "" {
		return noThread(gtx, s, "compact")
	}
	if c.op.run.Clicked(gtx) && !c.op.call.running() {
		c.op.start(s, request{endpoint: c.thread.Endpoint, method: "thread/compact/start", params: rpc.ThreadCompactStartParams{ThreadID: c.thread.Thread}})
	}
	if _, ok := c.op.settle(s); ok {
		s.env.Submit(state.Refresh{Thread: c.thread})
		s.announce(platform.AnnounceCompletion, "Context compacted")
		s.close()
	}
	content := append([]layout.Widget{
		s.para("Compact this thread? Earlier turns stay in the journal."),
		targetLine(s, c.thread),
	}, c.op.status(s, "Compacting")...)
	n, row := rows(content)
	return s.show(gtx, Content{
		Title: titles["compact"], Rows: n, Row: row, Dismiss: "Cancel",
		Actions: []layout.Widget{c.op.button(s, "Compact", "Compacting", Point)},
	})
}

type forkPanel struct {
	source turnSource
	list   turnList
	op     operation
}

func (f *forkPanel) open(r shell.Route) {
	f.source.reset(r.Thread)
	f.list.reset("")
	f.op.reset()
}

func (f *forkPanel) release(out *[]state.Intent) {
	f.source.release(out)
	f.op.call.release(out)
}

func (f *forkPanel) layout(gtx layout.Context, s *scene) layout.Dimensions {
	thread := f.source.thread
	if thread.Thread == "" {
		return noThread(gtx, s, "fork")
	}
	if f.source.update(gtx, s) {
		f.list.keep(f.source.turns)
	}
	f.list.update(gtx, f.source.turns)
	if f.op.run.Clicked(gtx) && !f.op.call.running() {
		f.confirm(s)
	}
	if v, ok := f.op.settle(s); ok {
		f.opened(s, v)
	}
	content := []layout.Widget{s.para("The fork keeps the thread up to the chosen turn and opens it."), targetLine(s, thread)}
	content = append(content, f.source.status(s)...)
	if f.source.loaded {
		content = append(content, f.list.rows(s, f.source.turns, "This thread has no turns to fork from.")...)
	}
	content = append(content, f.op.status(s, "Forking")...)
	f.op.run.Disabled = f.list.selected == ""
	n, row := rows(content)
	return s.show(gtx, Content{
		Title: titles["fork"], Rows: n, Row: row, Dismiss: "Cancel",
		Actions: []layout.Widget{f.op.button(s, "Fork here", "Forking", Point)},
	})
}

func (f *forkPanel) confirm(s *scene) {
	turn, ok := f.list.confirm(f.source.turns)
	if !ok {
		f.op.err = staleOrUnchosen(f.list.selected)
		return
	}
	thread := f.source.thread
	f.op.start(s, request{endpoint: thread.Endpoint, method: "thread/fork", params: rpc.ThreadForkParams{ThreadID: thread.Thread, LastTurnID: turn}})
}

func (f *forkPanel) opened(s *scene, v state.CallView) {
	forked, err := decode[rpc.ThreadForkResponse](v)
	if err != nil || forked.Thread.ID == "" {
		f.op.err = "The daemon answered the fork without a thread; open it from the thread list."
		return
	}
	s.announce(platform.AnnounceCompletion, "Fork opened")
	s.close()
	if s.env.Navigate != nil {
		s.env.Navigate(state.ThreadKey{Endpoint: f.source.thread.Endpoint, Thread: forked.Thread.ID})
	}
}

func staleOrUnchosen(selected string) string {
	if selected == "" {
		return chooseTurn
	}
	return staleTurn
}

type treePanel struct {
	source turnSource
	list   turnList
	op     operation
}

func (t *treePanel) open(r shell.Route) {
	t.source.reset(r.Thread)
	t.list.reset("")
	t.op.reset()
}

func (t *treePanel) release(out *[]state.Intent) {
	t.source.release(out)
	t.op.call.release(out)
}

func activeLeaf(turns []rpc.TreeTurn) string {
	for i := len(turns) - 1; i >= 0; i-- {
		turn := turns[i]
		extended := slices.ContainsFunc(turns, func(c rpc.TreeTurn) bool { return c.Active && c.ParentTurnID == turn.TurnID })
		if turn.Active && !extended {
			return turn.TurnID
		}
	}
	return ""
}

func (t *treePanel) layout(gtx layout.Context, s *scene) layout.Dimensions {
	thread := t.source.thread
	if thread.Thread == "" {
		return noThread(gtx, s, "tree")
	}
	first := !t.source.loaded
	if t.source.update(gtx, s) {
		t.list.keep(t.source.turns)
		if first && t.list.selected == "" {
			t.list.selected = activeLeaf(t.source.turns)
		}
	}
	t.list.update(gtx, t.source.turns)
	if t.op.run.Clicked(gtx) && !t.op.call.running() {
		t.confirm(s)
	}
	if _, ok := t.op.settle(s); ok {
		s.env.Submit(state.Refresh{Thread: thread})
		s.announce(platform.AnnounceCompletion, "Branch switched")
		s.close()
	}
	content := []layout.Widget{
		s.para("Choose the turn that the thread continues from."),
		s.note("Switching the branch changes the conversation path only; workspace files stay as they are."),
		targetLine(s, thread),
	}
	content = append(content, t.source.status(s)...)
	if t.source.loaded {
		content = append(content, t.branches(s)...)
	}
	content = append(content, t.op.status(s, "Switching branch")...)
	t.op.run.Disabled = t.list.selected == ""
	n, row := rows(content)
	return s.show(gtx, Content{
		Title: titles["tree"], Rows: n, Row: row, Dismiss: "Cancel",
		Actions: []layout.Widget{t.op.button(s, "Switch branch", "Switching", Point)},
	})
}

func (t *treePanel) branches(s *scene) []layout.Widget {
	turns := t.source.turns
	out := t.list.rows(s, turns, "This thread has no branches yet.")
	if len(turns) == 0 {
		return out
	}
	leaf := activeLeaf(turns)
	for i := range turns {
		row := out[i]
		relation := parentNote(turns, turns[i], leaf)
		out[i] = func(gtx layout.Context) layout.Dimensions {
			return layout.Flex{Axis: layout.Vertical}.Layout(gtx,
				layout.Rigid(row),
				layout.Rigid(func(gtx layout.Context) layout.Dimensions {
					return layout.Inset{Left: dpUnit(themeOptionIndent)}.Layout(gtx, s.note(relation))
				}),
			)
		}
	}
	return out
}

func parentNote(turns []rpc.TreeTurn, turn rpc.TreeTurn, leaf string) string {
	relation := "Starts the thread"
	if at := slices.IndexFunc(turns, func(p rpc.TreeTurn) bool { return p.TurnID == turn.ParentTurnID }); at >= 0 {
		relation = "Follows " + turnLabel(turns[at])
	}
	if turn.TurnID == leaf {
		relation += " · Current leaf"
	}
	return relation
}

func (t *treePanel) confirm(s *scene) {
	turn, ok := t.list.confirm(t.source.turns)
	if !ok {
		t.op.err = staleOrUnchosen(t.list.selected)
		return
	}
	thread := t.source.thread
	t.op.start(s, request{endpoint: thread.Endpoint, method: "dalgona/thread/leaf/set", params: rpc.ThreadLeafSetParams{ThreadID: thread.Thread, TurnID: turn}})
}

type rewindPanel struct {
	source turnSource
	list   turnList
	op     operation
}

func (r *rewindPanel) open(route shell.Route) {
	r.source.reset(route.Thread)
	r.list.reset(route.Turn)
	r.op.reset()
}

func (r *rewindPanel) release(out *[]state.Intent) {
	r.source.release(out)
	r.op.call.release(out)
}

func activePath(turns []rpc.TreeTurn) []rpc.TreeTurn {
	out := make([]rpc.TreeTurn, 0, len(turns))
	for _, t := range turns {
		if t.Active {
			out = append(out, t)
		}
	}
	return out
}

func (r *rewindPanel) layout(gtx layout.Context, s *scene) layout.Dimensions {
	thread := r.source.thread
	if thread.Thread == "" {
		return noThread(gtx, s, "revert")
	}
	if r.source.update(gtx, s) {
		r.list.keep(activePath(r.source.turns))
	}
	path := activePath(r.source.turns)
	r.list.update(gtx, path)
	if r.op.run.Clicked(gtx) && !r.op.call.running() {
		r.confirm(s, path)
	}
	if _, ok := r.op.settle(s); ok {
		s.env.Submit(state.Refresh{Thread: thread})
		s.announce(platform.AnnounceCompletion, "Conversation rewound")
		s.close()
	}
	content := []layout.Widget{targetLine(s, thread)}
	content = append(content, r.source.status(s)...)
	if r.source.loaded {
		content = append(content, r.list.rows(s, path, "No turn on the active path can be rewound.")...)
	}
	content = append(content, r.target(s, path)...)
	content = append(content, s.para("Rewind to before this turn? Later turns leave the active path. Files changed by later turns are not reverted."))
	content = append(content, r.op.status(s, "Rewinding")...)
	r.op.run.Disabled = r.list.selected == ""
	n, row := rows(content)
	return s.show(gtx, Content{
		Title: titles["revert"], Rows: n, Row: row, Dismiss: "Keep",
		Actions: []layout.Widget{r.op.button(s, "Rewind", "Rewinding", Point)},
	})
}

func (r *rewindPanel) target(s *scene, path []rpc.TreeTurn) []layout.Widget {
	at := slices.IndexFunc(path, func(t rpc.TreeTurn) bool { return t.TurnID == r.list.selected })
	if at < 0 {
		return nil
	}
	return []layout.Widget{s.legend("Rewind to before: " + turnLabel(path[at]))}
}

func (r *rewindPanel) confirm(s *scene, path []rpc.TreeTurn) {
	turn, ok := r.list.confirm(path)
	if !ok {
		r.op.err = staleOrUnchosen(r.list.selected)
		return
	}
	thread := r.source.thread
	r.op.start(s, request{endpoint: thread.Endpoint, method: "thread/revert", params: rpc.ThreadRevertParams{ThreadID: thread.Thread, BeforeTurnID: turn}})
}
