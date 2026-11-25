//go:build gui

package panel

import (
	"errors"
	"strconv"

	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type advisorPhase uint8

const (
	advisorLoading advisorPhase = iota
	advisorLoaded
	advisorUnavailable
	advisorUnreadable
)

var advisorWords = map[string]string{"idle": "Idle", "reviewing": "Reviewing", "halted": "Halted", "disabled": "Off"}

type advisorPanel struct {
	thread   state.ThreadKey
	status   pending
	phase    advisorPhase
	advisors []rpc.AdvisorState
	readErr  string
	retry    Button
	toggle   operation
	reset    operation
}

func (a *advisorPanel) open(r shell.Route) {
	*a = advisorPanel{thread: r.Thread}
}

func (a *advisorPanel) release(out *[]state.Intent) {
	a.status.release(out)
	a.toggle.call.release(out)
	a.reset.call.release(out)
}

func (a *advisorPanel) enabled() bool {
	for _, adv := range a.advisors {
		if adv.Status != "disabled" {
			return true
		}
	}
	return false
}

func (a *advisorPanel) read(s *scene) {
	a.phase, a.readErr = advisorLoading, ""
	a.status.start(s, request{endpoint: a.thread.Endpoint, method: "dalgona/advisor/status", params: rpc.DalgonaAdvisorStatusParams{ThreadID: a.thread.Thread}})
}

func (a *advisorPanel) settleStatus(s *scene) {
	v, done := a.status.settle(s)
	if !done {
		return
	}
	if v.State == state.CallSucceeded {
		list, err := decode[[]rpc.AdvisorState](v)
		if err == nil {
			a.phase, a.advisors = advisorLoaded, list
			return
		}
		v.Err = err
	}
	if coded, ok := errors.AsType[*rpc.RPCError](v.Err); ok && coded.Code == rpc.CodeNotAvailable {
		a.phase = advisorUnavailable
		return
	}
	a.phase, a.readErr = advisorUnreadable, failure(v)
}

func (a *advisorPanel) busy() bool {
	return a.toggle.call.running() || a.reset.call.running()
}

func (a *advisorPanel) act(gtx layout.Context, s *scene) {
	params := request{endpoint: a.thread.Endpoint}
	switch {
	case a.toggle.run.Clicked(gtx) && !a.busy():
		params.method, params.params = "dalgona/advisor/set", rpc.DalgonaAdvisorSetParams{ThreadID: a.thread.Thread, Enabled: !a.enabled()}
		a.reset.err = ""
		a.toggle.start(s, params)
	case a.reset.run.Clicked(gtx) && !a.busy():
		params.method, params.params = "dalgona/advisor/reset", rpc.DalgonaAdvisorStatusParams{ThreadID: a.thread.Thread}
		a.toggle.err = ""
		a.reset.start(s, params)
	}
	for _, op := range []*operation{&a.toggle, &a.reset} {
		if _, ok := op.settle(s); ok {
			s.announce(platform.AnnounceCompletion, "Advisor updated")
			a.read(s)
		}
	}
}

func (a *advisorPanel) layout(gtx layout.Context, s *scene) layout.Dimensions {
	if a.thread.Thread == "" {
		return noThread(gtx, s, "advisor")
	}
	if a.phase == advisorLoading && !a.status.running() {
		a.read(s)
	}
	if a.retry.Clicked(gtx) {
		a.read(s)
	}
	a.settleStatus(s)
	a.act(gtx, s)
	content := append([]layout.Widget{targetLine(s, a.thread)}, a.state(s)...)
	content = append(content, a.toggle.status(s, "Changing the advisor")...)
	content = append(content, a.reset.status(s, "Resetting the advisor")...)
	content = append(content, s.note("Advisor notes appear in the transcript."))
	label := "Turn advisor on"
	if a.enabled() {
		label = "Turn advisor off"
	}
	a.toggle.run.Disabled = a.phase != advisorLoaded
	a.reset.run.Disabled = a.phase != advisorLoaded || len(a.advisors) == 0
	n, row := rows(content)
	return s.show(gtx, Content{
		Title: titles["advisor"], Rows: n, Row: row,
		Actions: []layout.Widget{a.toggle.button(s, label, "Changing", Point), a.reset.button(s, "Reset advisor", "Resetting", Secondary)},
	})
}

func (a *advisorPanel) state(s *scene) []layout.Widget {
	switch a.phase {
	case advisorLoading:
		return []layout.Widget{s.note("Reading advisor status.")}
	case advisorUnavailable:
		return []layout.Widget{s.para("The advisor is unavailable on " + endpointName(s.env.View, a.thread.Endpoint) + ": its daemon runs no advisor.")}
	case advisorUnreadable:
		return []layout.Widget{
			s.alert("The advisor status could not be read: " + a.readErr),
			func(gtx layout.Context) layout.Dimensions {
				return a.retry.Layout(gtx, s.frame, s.env, "Try again", Secondary)
			},
		}
	case advisorLoaded:
	}
	if len(a.advisors) == 0 {
		return []layout.Widget{s.para("No advisor is attached to this thread.")}
	}
	out := make([]layout.Widget, 0, 2*len(a.advisors))
	for _, adv := range a.advisors {
		out = append(out, s.legend(adv.Name+" · "+adv.Model), s.note(advisorWord(adv.Status)+" · "+countNotes(adv.Notes)))
	}
	return out
}

func advisorWord(status string) string {
	if word, ok := advisorWords[status]; ok {
		return word
	}
	return status
}

func countNotes(n int) string {
	if n == 1 {
		return "1 note"
	}
	return strconv.Itoa(n) + " notes"
}
