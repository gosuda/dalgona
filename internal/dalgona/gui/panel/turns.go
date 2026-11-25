//go:build gui

package panel

import (
	"slices"

	"gioui.org/io/key"
	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	untitledTurn = "Untitled turn"
	activeMarker = "Active"
	staleTurn    = "The chosen turn is no longer in this thread. Choose another turn."
)

type turnList struct {
	choices  []Choice
	selected string
}

func turnLabel(t rpc.TreeTurn) string {
	if t.Preview == "" {
		return untitledTurn
	}
	return t.Preview
}

func (l *turnList) reset(preset string) {
	l.selected = preset
}

func (l *turnList) keep(turns []rpc.TreeTurn) {
	if !slices.ContainsFunc(turns, func(t rpc.TreeTurn) bool { return t.TurnID == l.selected }) {
		l.selected = ""
	}
}

func (l *turnList) confirm(turns []rpc.TreeTurn) (string, bool) {
	ok := l.selected != "" && slices.ContainsFunc(turns, func(t rpc.TreeTurn) bool { return t.TurnID == l.selected })
	return l.selected, ok
}

func (l *turnList) rows(s *scene, turns []rpc.TreeTurn, empty string) []layout.Widget {
	if len(turns) == 0 {
		return []layout.Widget{s.para(empty)}
	}
	for len(l.choices) < len(turns) {
		l.choices = append(l.choices, Choice{})
	}
	out := make([]layout.Widget, len(turns))
	for i := range turns {
		out[i] = func(gtx layout.Context) layout.Dimensions { return l.row(gtx, s, turns, i) }
	}
	return out
}

func (l *turnList) update(gtx layout.Context, turns []rpc.TreeTurn) {
	for i := range min(len(l.choices), len(turns)) {
		c := &l.choices[i]
		if c.Clicked(gtx) {
			l.selected = turns[i].TurnID
			gtx.Execute(key.FocusCmd{Tag: &c.Clickable})
		}
		if next, moved := l.step(gtx, c, i, len(turns)); moved {
			l.selected = turns[next].TurnID
			gtx.Execute(key.FocusCmd{Tag: &l.choices[next].Clickable})
		}
	}
}

func (l *turnList) row(gtx layout.Context, s *scene, turns []rpc.TreeTurn, i int) layout.Dimensions {
	o := Option{Label: turnLabel(turns[i]), Selected: turns[i].TurnID == l.selected}
	if turns[i].Active {
		o.Marker = activeMarker
	}
	return l.choices[i].Layout(gtx, s.frame, s.env, o)
}

func (l *turnList) step(gtx layout.Context, c *Choice, i, n int) (int, bool) {
	moved, next := false, i
	for {
		e, ok := gtx.Event(
			key.Filter{Focus: &c.Clickable, Name: key.NameDownArrow},
			key.Filter{Focus: &c.Clickable, Name: key.NameUpArrow},
		)
		if !ok {
			return next, moved
		}
		press, isKey := e.(key.Event)
		if !isKey || press.State != key.Press {
			continue
		}
		delta := 1
		if press.Name == key.NameUpArrow {
			delta = -1
		}
		if target := next + delta; target >= 0 && target < n {
			next, moved = target, true
		}
	}
}

type turnSource struct {
	call   pending
	thread state.ThreadKey
	turns  []rpc.TreeTurn
	loaded bool
	err    string
	retry  Button
}

func (t *turnSource) reset(thread state.ThreadKey) {
	*t = turnSource{thread: thread}
}

func (t *turnSource) load(s *scene) {
	t.err = ""
	t.call.start(s, request{endpoint: t.thread.Endpoint, method: "dalgona/thread/tree", params: rpc.ThreadTreeParams{ThreadID: t.thread.Thread}})
}

func (t *turnSource) update(gtx layout.Context, s *scene) bool {
	if !t.loaded && !t.call.running() && t.err == "" {
		t.load(s)
	}
	if t.retry.Clicked(gtx) {
		t.load(s)
	}
	v, done := t.call.settle(s)
	if !done {
		return false
	}
	if v.State != state.CallSucceeded {
		t.err = failure(v)
		return false
	}
	tree, err := decode[rpc.ThreadTreeResponse](v)
	if err != nil {
		t.err = err.Error()
		return false
	}
	t.turns, t.loaded = tree.Turns, true
	return true
}

func (t *turnSource) status(s *scene) []layout.Widget {
	switch {
	case t.err != "":
		return []layout.Widget{
			s.alert("Unable to load the turns: " + t.err),
			func(gtx layout.Context) layout.Dimensions {
				return t.retry.Layout(gtx, s.frame, s.env, "Try again", Secondary)
			},
		}
	case !t.loaded:
		return []layout.Widget{s.note("Loading turns…")}
	}
	return nil
}

func (t *turnSource) release(out *[]state.Intent) {
	t.call.release(out)
}
