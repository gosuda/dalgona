//go:build gui

package shell

import (
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
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	testWindow = state.WindowID("window-1")
	local      = client.EndpointID("/home/u/.dalgona")
	remote     = client.EndpointID("lab")
)

type focusPart struct {
	label  string
	height int
	tag    widget.Clickable
	envs   []Env
}

func (p *focusPart) Layout(gtx layout.Context, env Env) layout.Dimensions {
	p.envs = append(p.envs, env)
	return p.tag.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.LabelOp(p.label).Add(gtx.Ops)
		return layout.Dimensions{Size: image.Pt(gtx.Constraints.Max.X, p.height)}
	})
}

func (p *focusPart) FocusTag() event.Tag { return &p.tag }

type requestPart struct {
	showing  bool
	released bool
	composer *focusPart
	before   []bool
}

func (r *requestPart) Layout(gtx layout.Context, env Env) layout.Dimensions {
	r.before = append(r.before, r.composer == nil || len(r.composer.envs) < len(r.before)+1)
	return layout.Dimensions{}
}

func (r *requestPart) Showing() bool { return r.showing }

func (r *requestPart) FocusReleased() bool {
	released := r.released
	r.released = false
	return released
}

type panelPart struct {
	name   string
	opened []Route
	done   widget.Clickable
	env    Env
}

func (p *panelPart) Opened(r Route) { p.opened = append(p.opened, r) }

func (p *panelPart) Layout(gtx layout.Context, env Env) layout.Dimensions {
	p.env = env
	if p.done.Clicked(gtx) {
		env.Close()
	}
	return p.done.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.Button.Add(gtx.Ops)
		semantic.LabelOp("Close " + p.name).Add(gtx.Ops)
		return layout.Dimensions{Size: image.Pt(120, 40)}
	})
}

type settlerPart struct {
	windows []state.WindowID
}

func (s *settlerPart) Settle(env Env) { s.windows = append(s.windows, env.Window) }

type harness struct {
	t          *testing.T
	shell      *Shell
	router     *input.Router
	now        time.Time
	intents    []state.Intent
	announced  []platform.Announcement
	transcript *focusPart
	composer   *focusPart
	request    *requestPart
	panels     map[string]*panelPart
	settler    *settlerPart
	nodes      []input.SemanticNode
	ax         *accesstest.Frame
	size       image.Point
	windows    struct{ opened, closed int }
}

var panelNames = []string{"help", "model", "new", "rename", "archive", "archived", "revert", "settings", "doctor"}

func newHarness(t *testing.T) *harness {
	t.Helper()
	shaper, err := platform.NewShaper()
	if err != nil {
		t.Fatal(err)
	}
	h := &harness{
		t: t, router: new(input.Router), now: time.Unix(1_800_000_000, 0),
		transcript: &focusPart{label: "fake transcript", height: 200},
		composer:   &focusPart{label: "fake composer", height: 80},
		request:    &requestPart{},
		panels:     map[string]*panelPart{},
		size:       image.Pt(1280, 800),
	}
	h.request.composer = h.composer
	h.settler = &settlerPart{}
	parts := Parts{Transcript: h.transcript, Composer: h.composer, Request: h.request, Panels: map[string]Panel{}, Settlers: []Settler{h.settler}}
	for _, name := range panelNames {
		p := &panelPart{name: name}
		h.panels[name] = p
		parts.Panels[name] = p
	}
	h.shell = New(Config{
		Shaper: shaper, Window: testWindow, Parts: parts,
		Submit:      func(i state.Intent) { h.intents = append(h.intents, i) },
		Announce:    func(n platform.Announcement) { h.announced = append(h.announced, n) },
		NewWindow:   func() { h.windows.opened++ },
		CloseWindow: func() { h.windows.closed++ },
	})
	return h
}

func (h *harness) frame(v *state.View) bool {
	h.t.Helper()
	h.now = h.now.Add(16 * time.Millisecond)
	ops := new(op.Ops)
	gtx := layout.Context{
		Ops:         ops,
		Now:         h.now,
		Metric:      unit.Metric{PxPerDp: 1, PxPerSp: 1},
		Constraints: layout.Exact(h.size),
		Source:      h.router.Source(),
	}
	gtx = h.ax.Begin(gtx)
	confirmed := h.shell.Layout(gtx, Frame{Theme: theme.Night, View: v, Still: true})
	h.router.Frame(ops)
	h.ax.End(h.t, gtx)
	h.nodes = h.router.AppendSemantics(nil)
	return confirmed
}

func (h *harness) settle(v *state.View) {
	h.t.Helper()
	h.frame(v)
	h.frame(v)
}

func (h *harness) take() []state.Intent {
	out := h.intents
	h.intents = nil
	return out
}

func (h *harness) button(label string) (input.SemanticNode, bool) {
	for _, n := range h.nodes {
		if n.Desc.Class == semantic.Button && n.Desc.Label == label {
			return n, true
		}
	}
	return input.SemanticNode{}, false
}

func (h *harness) click(v *state.View, label string) {
	h.t.Helper()
	n, ok := h.button(label)
	if !ok {
		h.t.Fatalf("no button named %q among %v", label, h.labels())
	}
	h.clickAt(n.Desc.Bounds)
	h.frame(v)
}

func (h *harness) clickAt(b image.Rectangle) {
	at := f32.Pt(float32(b.Min.X+b.Max.X)/2, float32(b.Min.Y+b.Max.Y)/2)
	h.router.Queue(
		pointer.Event{Kind: pointer.Press, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at},
		pointer.Event{Kind: pointer.Release, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at},
	)
}

func (h *harness) press(v *state.View, name key.Name, mods key.Modifiers) {
	h.router.Queue(key.Event{Name: name, Modifiers: mods, State: key.Press}, key.Event{Name: name, Modifiers: mods, State: key.Release})
	h.frame(v)
}

func (h *harness) focus(v *state.View, tag event.Tag) {
	h.router.Queue()
	ops := new(op.Ops)
	gtx := layout.Context{Ops: ops, Now: h.now, Metric: unit.Metric{PxPerDp: 1, PxPerSp: 1}, Constraints: layout.Exact(h.size), Source: h.router.Source()}
	gtx.Execute(key.FocusCmd{Tag: tag})
	h.frame(v)
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

func (h *harness) region(label string) image.Rectangle {
	h.t.Helper()
	for _, n := range h.nodes {
		if n.Desc.Label == label {
			return n.Desc.Bounds
		}
	}
	h.t.Fatalf("no semantic node labelled %q", label)
	return image.Rectangle{}
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

func endpoint(id client.EndpointID, label string, phase client.Phase) state.EndpointView {
	kind := client.EndpointDaemon
	if id == local {
		kind = client.EndpointLocal
	}
	return state.EndpointView{ID: id, Label: label, Kind: kind, Phase: phase, Live: phase == client.PhaseConnected}
}

func summary(id, name, preview string) rpc.Thread {
	return rpc.Thread{ID: id, Name: name, Preview: preview, Model: "gpt-test", UpdatedAt: 1_790_000_000, Status: rpc.ThreadStatus{Type: "idle"}}
}

func loaded(rows ...rpc.Thread) state.LaneView {
	return state.LaneView{Rows: rows, Loaded: true}
}

func openThread(key state.ThreadKey, title string, turns ...state.TurnView) *state.ThreadView {
	return &state.ThreadView{Key: key, Live: true, HistoryID: "h-" + key.Thread, Thread: rpc.Thread{ID: key.Thread, Name: title}, Turns: turns}
}

func connectedView(rows ...rpc.Thread) state.Snapshot {
	return state.Snapshot{
		Endpoints: []state.EndpointView{endpoint(local, "local", client.PhaseConnected)},
		Lanes:     map[client.EndpointID]state.LaneView{local: loaded(rows...)},
	}
}
