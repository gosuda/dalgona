//go:build gui

package panel

import (
	"encoding/json/v2"
	"errors"
	"slices"
	"strconv"
	"sync/atomic"

	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

var (
	errNoResult = errors.New("the daemon answered without a result")
	callSerial  atomic.Uint64
)

type scene struct {
	env   shell.Env
	frame *Frame
	out   *[]state.Intent
	close func()
}

func (s *scene) show(gtx layout.Context, c Content) layout.Dimensions {
	if c.OnDismiss == nil {
		c.OnDismiss = s.close
	}
	return s.frame.Layout(gtx, s.env, c)
}

func rows(widgets []layout.Widget) (int, layout.ListElement) {
	return len(widgets), func(gtx layout.Context, i int) layout.Dimensions { return widgets[i](gtx) }
}

type request struct {
	endpoint  client.EndpointID
	method    string
	params    any
	operation bool
}

type pending struct {
	id state.CallID
}

func mint() state.CallID {
	return state.CallID("panel/" + strconv.FormatUint(callSerial.Add(1), 10))
}

func (s *scene) announce(kind platform.AnnouncementKind, text string) {
	if s.env.Announce != nil {
		s.env.Announce(platform.Announcement{Kind: kind, Text: text})
	}
}

func (c *pending) start(s *scene, r request) {
	c.release(s.out)
	c.id = mint()
	s.env.Submit(state.Call{ID: c.id, Endpoint: r.endpoint, Method: r.method, Params: r.params, Operation: r.operation})
}

func (c *pending) running() bool { return c.id != "" }

func (c *pending) peek(s *scene) (state.CallView, bool) {
	if c.id == "" {
		return state.CallView{}, false
	}
	v, ok := s.env.View.Call(c.id)
	if !ok {
		return state.CallView{ID: c.id, State: state.CallRunning}, true
	}
	return v, true
}

func (c *pending) settle(s *scene) (state.CallView, bool) {
	v, ok := c.peek(s)
	if !ok || v.State == state.CallRunning || v.State == state.CallUnknown {
		return state.CallView{}, false
	}
	c.release(s.out)
	return v, true
}

func (c *pending) unknown(s *scene) bool {
	v, ok := c.peek(s)
	return ok && v.State == state.CallUnknown
}

func (c *pending) release(out *[]state.Intent) {
	if c.id == "" {
		return
	}
	*out = append(*out, state.CancelCall{ID: c.id})
	c.id = ""
}

func decode[T any](v state.CallView) (T, error) {
	var out T
	if len(v.Result) == 0 {
		return out, errNoResult
	}
	if err := json.Unmarshal(v.Result, &out); err != nil {
		return out, err
	}
	return out, nil
}

func failure(v state.CallView) string {
	if v.Err == nil {
		return "the daemon reported a failure without a cause"
	}
	return v.Err.Error()
}

func endpointView(v *state.View, id client.EndpointID) (state.EndpointView, bool) {
	eps := v.Endpoints()
	at := slices.IndexFunc(eps, func(e state.EndpointView) bool { return e.ID == id })
	if at < 0 {
		return state.EndpointView{ID: id}, false
	}
	return eps[at], true
}

func endpointName(v *state.View, id client.EndpointID) string {
	if ep, _ := endpointView(v, id); ep.Label != "" {
		return ep.Label
	}
	return string(id)
}

func defaultEndpoint(env shell.Env, preferred client.EndpointID) client.EndpointID {
	if preferred != "" {
		return preferred
	}
	if env.Thread.Endpoint != "" {
		return env.Thread.Endpoint
	}
	eps := env.View.Endpoints()
	if at := slices.IndexFunc(eps, func(e state.EndpointView) bool { return e.Kind == client.EndpointLocal }); at >= 0 {
		return eps[at].ID
	}
	if len(eps) > 0 {
		return eps[0].ID
	}
	return ""
}

var phaseWords = [...]string{
	client.PhaseIdle:         "Idle",
	client.PhaseConnecting:   "Connecting",
	client.PhaseConnected:    "Connected",
	client.PhaseReconnecting: "Reconnecting",
	client.PhaseOffline:      "Offline",
	client.PhaseIncompatible: "Incompatible",
	client.PhaseFailed:       "Failed",
}

func phaseWord(p client.Phase) string {
	if int(p) < len(phaseWords) {
		return phaseWords[p]
	}
	return p.String()
}

func threadTitle(t *state.ThreadView) string {
	switch {
	case t == nil:
		return "this thread"
	case t.Thread.Name != "":
		return t.Thread.Name
	case t.Thread.Preview != "":
		return t.Thread.Preview
	}
	return "Untitled thread"
}

func decodeCall[T any](v state.CallView) (T, error) {
	if v.State != state.CallSucceeded {
		var zero T
		if v.Err != nil {
			return zero, v.Err
		}
		return zero, errors.New(failure(v))
	}
	return decode[T](v)
}
