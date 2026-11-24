//go:build gui

package decision

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"image"
	"slices"
	"strings"
	"testing"
	"time"

	"gioui.org/f32"
	"gioui.org/io/input"
	"gioui.org/io/key"
	"gioui.org/io/pointer"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
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

var (
	openThread = state.ThreadKey{Endpoint: "/home/u/.dalgona", Thread: "t1"}
	liveLink   = client.Link{Endpoint: "/home/u/.dalgona", Generation: 1}
)

type harness struct {
	t         *testing.T
	dock      *Dock
	router    *input.Router
	shaper    *text.Shaper
	now       time.Time
	size      image.Point
	modal     bool
	last      scene
	ime       inputMethod
	submitted []state.Intent
	announced []platform.Announcement
	nodes     []input.SemanticNode
	ax        *accesstest.Frame
	clipboard func() platform.ClipboardReader
	woke      chan struct{}
}

func newHarness(t *testing.T) *harness {
	t.Helper()
	shaper, err := platform.NewShaper()
	if err != nil {
		t.Fatal(err)
	}
	h := &harness{t: t, dock: NewDock(), router: new(input.Router), shaper: shaper, now: time.Unix(1_800_000_000, 0), size: image.Pt(720, 2400), woke: make(chan struct{}, 4)}
	h.ime = inputMethod{router: h.router, compose: noComposition}
	return h
}

func (h *harness) env() shell.Env {
	return shell.Env{
		Theme: theme.Night, Shaper: h.shaper, Window: "w1", Thread: openThread, Modal: h.modal, Clipboard: h.clipboard,
		Wake:     func() { h.woke <- struct{}{} },
		Submit:   func(i state.Intent) { h.submitted = append(h.submitted, i) },
		Announce: func(a platform.Announcement) { h.announced = append(h.announced, a) },
	}
}

func (h *harness) frame(sc scene) layout.Dimensions {
	h.t.Helper()
	h.now = h.now.Add(16 * time.Millisecond)
	ops := new(op.Ops)
	gtx := layout.Context{
		Ops: ops, Now: h.now, Metric: unit.Metric{PxPerDp: 1, PxPerSp: 1},
		Constraints: layout.Constraints{Max: h.size}, Source: h.router.Source(),
	}
	gtx = h.ax.Begin(gtx)
	dims := h.dock.layoutScene(gtx, h.env(), sc)
	h.router.Frame(ops)
	h.ax.End(h.t, gtx)
	h.ime.state = h.router.EditorState()
	h.nodes = h.router.AppendSemantics(h.nodes[:0])
	return dims
}

func (h *harness) node(label string, class semantic.ClassOp) (input.SemanticNode, bool) {
	for _, n := range h.nodes {
		if n.Desc.Label == label && n.Desc.Class == class {
			return n, true
		}
	}
	return input.SemanticNode{}, false
}

func (h *harness) must(label string, class semantic.ClassOp) input.SemanticNode {
	h.t.Helper()
	n, ok := h.node(label, class)
	if !ok {
		h.t.Fatalf("no %v node labelled %q among %v", class, label, h.labels())
	}
	return n
}

func (h *harness) labels() []string {
	out := make([]string, 0, len(h.nodes))
	for _, n := range h.nodes {
		if n.Desc.Label != "" {
			out = append(out, n.Desc.Label)
		}
	}
	return out
}

func (h *harness) shows(text string) bool {
	return slices.ContainsFunc(h.nodes, func(n input.SemanticNode) bool { return strings.Contains(n.Desc.Label, text) })
}

func (h *harness) showsExactly(text string) bool {
	return slices.ContainsFunc(h.nodes, func(n input.SemanticNode) bool { return n.Desc.Label == text })
}

func (h *harness) click(label string, class semantic.ClassOp) {
	h.t.Helper()
	b := h.must(label, class).Desc.Bounds
	at := f32.Pt(float32(b.Min.X+b.Max.X)/2, float32(b.Min.Y+b.Max.Y)/2)
	h.router.Queue(
		pointer.Event{Kind: pointer.Press, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at, Time: h.now.Sub(time.Unix(0, 0))},
		pointer.Event{Kind: pointer.Release, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at, Time: h.now.Sub(time.Unix(0, 0)) + 50*time.Millisecond},
	)
}

func (h *harness) press(name key.Name) {
	h.router.Queue(key.Event{Name: name, State: key.Press}, key.Event{Name: name, State: key.Release})
}

func (h *harness) answers() []state.Answer {
	var out []state.Answer
	for _, i := range h.submitted {
		if a, ok := i.(state.Answer); ok {
			out = append(out, a)
		}
	}
	return out
}

func (h *harness) onlyAnswer() state.Answer {
	h.t.Helper()
	got := h.answers()
	if len(got) != 1 {
		h.t.Fatalf("submitted %d answers, want exactly 1: %v", len(got), h.submitted)
	}
	return got[0]
}

func decisionOf(t *testing.T, result jsontext.Value) string {
	t.Helper()
	var r rpc.CommandExecutionRequestApprovalResponse
	if err := json.Unmarshal(result, &r); err != nil {
		t.Fatalf("decision result %s: %v", result, err)
	}
	return r.Decision
}

func answersOf(t *testing.T, result jsontext.Value) map[string][]string {
	t.Helper()
	var r struct {
		Answers map[string]struct {
			Answers []string `json:"answers"`
		} `json:"answers"`
	}
	if err := json.Unmarshal(result, &r, json.RejectUnknownMembers(true)); err != nil {
		t.Fatalf("answer result %s: %v", result, err)
	}
	if r.Answers == nil {
		t.Fatalf("answer result %s has no answers object", result)
	}
	out := make(map[string][]string, len(r.Answers))
	for id, a := range r.Answers {
		if a.Answers == nil {
			t.Fatalf("answer result %s has no answers list for %s", result, id)
		}
		out[id] = a.Answers
	}
	return out
}

func request(id, method string, arrival uint64, params string) state.RequestView {
	return state.RequestView{
		Key: state.RequestKey{Thread: openThread, Request: id}, Method: method, Params: jsontext.Value(params),
		Actionable: true, Arrival: arrival,
	}
}

func thread(requests ...state.RequestView) *state.ThreadView {
	return &state.ThreadView{Key: openThread, Live: true, Link: liveLink, Requests: requests}
}

func sceneWith(tv *state.ThreadView, others ...*state.ThreadView) scene {
	lookup := func(k state.ThreadKey) (*state.ThreadView, bool) {
		for _, o := range append([]*state.ThreadView{tv}, others...) {
			if o.Key == k {
				return o, true
			}
		}
		return nil, false
	}
	return collect(tv, lookup, "box")
}

const commandParams = `{"threadId":"t1","turnId":"u1","itemId":"c1","startedAtMs":1,"command":"git  commit -m \"fix: a  b\" && rm -rf build","cwd":"/work/repo","reason":"commit the staged fix"}`
