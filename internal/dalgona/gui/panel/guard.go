//go:build gui

package panel

import (
	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

var titles = map[string]string{
	"help": "Keys", "model": "Choose a model", "settings": "Settings", "login": "Log in",
	"resume": "Open an earlier thread", "new": "Start a new thread", "compact": "Compact the context",
	"fork": "Fork at a turn", "tree": "Move to another branch", "revert": "Rewind to a turn",
	"advisor": "Advisor", "review": "Review changes",
}

type refusal struct {
	panel     string
	thread    state.ThreadKey
	turn      string
	interrupt Button
	sent      bool
}

func refuse(env shell.Env, r shell.Route) refusal {
	if !refusedWhileRunning[r.Panel] {
		return refusal{}
	}
	t, ok := env.View.Thread(r.Thread)
	if !ok || t.ActiveTurn == "" {
		return refusal{}
	}
	return refusal{panel: r.Panel, thread: r.Thread, turn: t.ActiveTurn}
}

func (g *refusal) running(v *state.View) bool {
	t, ok := v.Thread(g.thread)
	return ok && t.ActiveTurn == g.turn
}

func (g *refusal) layout(gtx layout.Context, s *scene) layout.Dimensions {
	running := g.running(s.env.View)
	if running && g.interrupt.Clicked(gtx) {
		g.sent = true
		s.env.Submit(state.Interrupt{Window: s.env.Window, Thread: g.thread, Turn: g.turn})
		s.announce(platform.AnnounceProgress, "Interrupting the running turn")
	}
	g.interrupt.Busy = ""
	if g.sent {
		g.interrupt.Busy = "Interrupting"
	}
	t, _ := s.env.View.Thread(g.thread)
	where := threadTitle(t) + " on " + endpointName(s.env.View, g.thread.Endpoint)
	command := "/" + g.panel
	content := []layout.Widget{
		s.para(command + " is unavailable while a turn is working; wait for it to finish or interrupt it first."),
		s.note("Turn " + g.turn + " is working in " + where + "."),
	}
	var actions []layout.Widget
	if running {
		actions = append(actions, func(gtx layout.Context) layout.Dimensions {
			return g.interrupt.Layout(gtx, s.frame, s.env, "Interrupt", Point)
		})
	} else {
		content = []layout.Widget{s.para("The turn has stopped. Choose " + command + " again to continue.")}
	}
	n, row := rows(content)
	return s.show(gtx, Content{Title: titles[g.panel], Rows: n, Row: row, Actions: actions})
}
