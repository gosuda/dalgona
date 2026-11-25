//go:build gui

package panel

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"image"
	"slices"
	"strings"
	"testing"
	"time"

	"gioui.org/f32"
	"gioui.org/io/event"
	"gioui.org/io/input"
	"gioui.org/io/key"
	"gioui.org/io/pointer"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/text"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	local  client.EndpointID = "/home/test/.dalgona"
	box    client.EndpointID = "box"
	window state.WindowID    = "w1"
)

var (
	sharedShaper *text.Shaper
	threadA      = state.ThreadKey{Endpoint: local, Thread: "thread-a"}
	threadB      = state.ThreadKey{Endpoint: box, Thread: "thread-b"}
)

type harness struct {
	t         *testing.T
	panels    *Panels
	router    *input.Router
	now       time.Time
	size      image.Point
	snap      state.Snapshot
	calls     map[state.CallID]state.CallView
	thread    state.ThreadKey
	intents   []state.Intent
	closed    int
	navigated []state.ThreadKey
	opened    []shell.Route
	announced []platform.Announcement
	nodes     []input.SemanticNode
	ax        *accesstest.Frame
	dialogs   *fakeDialogs
	outside   widgetTag
}

type widgetTag struct{ _ int }

type fakeDialogs struct {
	requests []platform.DialogRequest
	err      error
}

func (d *fakeDialogs) ShowDialog(req platform.DialogRequest) error {
	d.requests = append(d.requests, req)
	return d.err
}

func shaper(t *testing.T) *text.Shaper {
	t.Helper()
	if sharedShaper == nil {
		s, err := platform.NewShaper()
		if err != nil {
			t.Fatal(err)
		}
		sharedShaper = s
	}
	return sharedShaper
}

func newHarness(t *testing.T, endpoints ...state.EndpointView) *harness {
	t.Helper()
	if len(endpoints) == 0 {
		endpoints = []state.EndpointView{endpointOf(local, "local", client.PhaseConnected)}
	}
	h := &harness{
		t: t, router: new(input.Router), now: time.Unix(1_800_000_000, 0), size: image.Pt(1000, 4000),
		calls: map[state.CallID]state.CallView{}, dialogs: &fakeDialogs{},
		snap: state.Snapshot{Endpoints: endpoints, Lanes: map[client.EndpointID]state.LaneView{}},
	}
	h.panels = New(nil, h.dialogs)
	shaper(t)
	return h
}

func endpointOf(id client.EndpointID, label string, phase client.Phase) state.EndpointView {
	kind := client.EndpointDaemon
	if id == local {
		kind = client.EndpointLocal
	}
	return state.EndpointView{ID: id, Label: label, Kind: kind, Phase: phase, Live: phase == client.PhaseConnected}
}

func (h *harness) view() *state.View {
	snap := h.snap
	snap.Calls = nil
	for _, c := range h.calls {
		snap.Calls = append(snap.Calls, c)
	}
	return snap.View()
}

func (h *harness) env() shell.Env {
	return shell.Env{
		Theme: theme.Night, Shaper: sharedShaper, View: h.view(), Window: window, Thread: h.thread, Modal: true,
		Submit:   func(i state.Intent) { h.intents = append(h.intents, i) },
		Navigate: func(k state.ThreadKey) { h.navigated = append(h.navigated, k) },
		Open:     func(r shell.Route) { h.opened = append(h.opened, r) },
		Close:    func() { h.closed++ },
		Announce: func(a platform.Announcement) { h.announced = append(h.announced, a) },
	}
}

func (h *harness) open(r shell.Route) {
	h.t.Helper()
	h.panels.Opened(r)
	h.settle()
}

func (h *harness) gtx(ops *op.Ops) layout.Context {
	return layout.Context{
		Ops: ops, Now: h.now, Metric: unit.Metric{PxPerDp: 1, PxPerSp: 1},
		Constraints: layout.Exact(h.size), Source: h.router.Source(),
	}
}

func (h *harness) frame() {
	h.t.Helper()
	h.now = h.now.Add(16 * time.Millisecond)
	ops := new(op.Ops)
	gtx := h.ax.Begin(h.gtx(ops))
	root := clip.Rect{Max: h.size}.Push(gtx.Ops)
	event.Op(gtx.Ops, &h.outside)
	for {
		if _, ok := gtx.Event(key.FocusFilter{Target: &h.outside}); !ok {
			break
		}
	}
	env := h.env()
	h.panels.Settle(env)
	h.panels.Layout(gtx, env)
	root.Pop()
	h.router.Frame(ops)
	h.ax.End(h.t, gtx)
	h.nodes = h.router.AppendSemantics(nil)
}

func (h *harness) settle() {
	h.t.Helper()
	h.frame()
	h.frame()
}

func (h *harness) wait(d time.Duration) {
	h.t.Helper()
	h.now = h.now.Add(d)
	h.settle()
}

func (h *harness) take() []state.Intent {
	out := h.intents
	h.intents = nil
	return out
}

func (h *harness) node(class semantic.ClassOp, label string) (input.SemanticNode, bool) {
	at := slices.IndexFunc(h.nodes, func(n input.SemanticNode) bool { return n.Desc.Class == class && n.Desc.Label == label })
	if at < 0 {
		return input.SemanticNode{}, false
	}
	return h.nodes[at], true
}

func (h *harness) button(label string) (input.SemanticNode, bool) {
	return h.node(semantic.Button, label)
}

func (h *harness) clickNode(class semantic.ClassOp, label string) {
	h.t.Helper()
	n, ok := h.node(class, label)
	if !ok {
		h.t.Fatalf("no %v named %q among %q", class, label, h.labels())
	}
	b := n.Desc.Bounds
	at := f32.Pt(float32(b.Min.X+b.Max.X)/2, float32(b.Min.Y+b.Max.Y)/2)
	h.router.Queue(
		pointer.Event{Kind: pointer.Press, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at},
		pointer.Event{Kind: pointer.Release, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at},
	)
	h.settle()
}

func (h *harness) click(label string) { h.t.Helper(); h.clickNode(semantic.Button, label) }

func (h *harness) choose(label string) { h.t.Helper(); h.clickNode(semantic.RadioButton, label) }

func (h *harness) press(name key.Name, mods key.Modifiers) {
	h.t.Helper()
	h.router.Queue(key.Event{Name: name, Modifiers: mods, State: key.Press}, key.Event{Name: name, Modifiers: mods, State: key.Release})
	h.settle()
}

func (h *harness) focus(tag event.Tag) {
	h.t.Helper()
	gtx := h.gtx(new(op.Ops))
	gtx.Execute(key.FocusCmd{Tag: tag})
	h.settle()
}

func (h *harness) typeInto(f *field, text string) {
	h.t.Helper()
	h.focus(f.editor.Tag())
	at := len([]rune(f.text()))
	h.router.Queue(key.EditEvent{Range: key.Range{Start: at, End: at}, Text: text})
	h.settle()
}

func (h *harness) focused(tag event.Tag) bool {
	return h.router.Source().Focused(tag)
}

func (h *harness) labels() []string {
	var out []string
	for _, n := range h.nodes {
		if n.Desc.Label != "" {
			out = append(out, n.Desc.Label)
		}
	}
	return out
}

func (h *harness) has(fragment string) bool {
	return slices.ContainsFunc(h.labels(), func(l string) bool { return strings.Contains(l, fragment) })
}

func (h *harness) want(fragments ...string) {
	h.t.Helper()
	for _, f := range fragments {
		if !h.has(f) {
			h.t.Errorf("missing %q among %q", f, h.labels())
		}
	}
}

func (h *harness) reject(fragments ...string) {
	h.t.Helper()
	for _, f := range fragments {
		if h.has(f) {
			h.t.Errorf("unexpected %q among %q", f, h.labels())
		}
	}
}

func (h *harness) call(method string) state.Call {
	h.t.Helper()
	calls := submitted[state.Call](h.intents)
	for i := len(calls) - 1; i >= 0; i-- {
		if calls[i].Method == method {
			return calls[i]
		}
	}
	h.t.Fatalf("no %s call among %v", method, h.intents)
	return state.Call{}
}

func (h *harness) methods() []string {
	var out []string
	for _, c := range submitted[state.Call](h.intents) {
		out = append(out, c.Method)
	}
	return out
}

func (h *harness) answer(c state.Call, result any) {
	h.t.Helper()
	raw, err := json.Marshal(result)
	if err != nil {
		h.t.Fatal(err)
	}
	h.calls[c.ID] = state.CallView{ID: c.ID, Endpoint: c.Endpoint, Generation: 1, State: state.CallSucceeded, Result: jsontext.Value(raw)}
	h.settle()
}

func (h *harness) fail(c state.Call, cause string) {
	h.t.Helper()
	h.calls[c.ID] = state.CallView{ID: c.ID, Endpoint: c.Endpoint, Generation: 1, State: state.CallFailed, Err: errors.New(cause)}
	h.settle()
}

func (h *harness) running(c state.Call) {
	h.calls[c.ID] = state.CallView{ID: c.ID, Endpoint: c.Endpoint, Generation: 1, State: state.CallRunning}
}

func submitted[T state.Intent](intents []state.Intent) []T {
	var out []T
	for _, i := range intents {
		if v, ok := i.(T); ok {
			out = append(out, v)
		}
	}
	return out
}

func params[T any](t *testing.T, c state.Call) T {
	t.Helper()
	raw, err := json.Marshal(c.Params)
	if err != nil {
		t.Fatal(err)
	}
	var out T
	if err := json.Unmarshal(raw, &out); err != nil {
		t.Fatal(err)
	}
	return out
}

func rawParams(t *testing.T, c state.Call) map[string]any {
	t.Helper()
	return params[map[string]any](t, c)
}

func idleThread(key state.ThreadKey, title string) *state.ThreadView {
	return &state.ThreadView{Key: key, Live: true, HistoryID: "h-" + key.Thread, Thread: rpc.Thread{ID: key.Thread, Name: title}}
}

func runningThread(key state.ThreadKey, title, turn string) *state.ThreadView {
	t := idleThread(key, title)
	t.ActiveTurn = turn
	return t
}
