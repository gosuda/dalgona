//go:build gui

package composer

import (
	"context"
	"image"
	"strings"
	"testing"
	"time"
	"unicode/utf8"

	"gioui.org/f32"
	"gioui.org/io/input"
	"gioui.org/io/key"
	"gioui.org/io/pointer"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
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
	testEndpoint client.EndpointID = "laptop"
	testWindow   state.WindowID    = "w1"
	testThread                     = "thread-1"
)

var (
	testKey       = state.DraftKey{Endpoint: testEndpoint, Window: testWindow, Thread: testThread}
	testThreadKey = state.ThreadKey{Endpoint: testEndpoint, Thread: testThread}
)

type fakeView struct {
	drafts    map[state.DraftKey]state.Draft
	followUps map[state.DraftKey][]state.FollowUp
	threads   map[state.ThreadKey]*state.ThreadView
	keywords  map[state.DraftKey]state.DraftKeywords
	searches  map[state.DraftKey]state.FileSearch
	runs      map[state.DraftKey][]state.ShellRun
	images    map[state.ThreadKey]bool
	prompts   map[state.ThreadKey][]string
}

func newFakeView() *fakeView {
	return &fakeView{
		drafts: map[state.DraftKey]state.Draft{}, followUps: map[state.DraftKey][]state.FollowUp{},
		threads: map[state.ThreadKey]*state.ThreadView{}, keywords: map[state.DraftKey]state.DraftKeywords{},
		searches: map[state.DraftKey]state.FileSearch{}, runs: map[state.DraftKey][]state.ShellRun{},
		images: map[state.ThreadKey]bool{}, prompts: map[state.ThreadKey][]string{},
	}
}

func (v *fakeView) Draft(k state.DraftKey) state.Draft                 { return v.drafts[k] }
func (v *fakeView) FollowUps(k state.DraftKey) []state.FollowUp        { return v.followUps[k] }
func (v *fakeView) DraftKeywords(k state.DraftKey) state.DraftKeywords { return v.keywords[k] }
func (v *fakeView) FileSearch(k state.DraftKey) state.FileSearch       { return v.searches[k] }
func (v *fakeView) ShellRuns(k state.DraftKey) []state.ShellRun        { return v.runs[k] }
func (v *fakeView) Prompts(k state.ThreadKey) []string                 { return v.prompts[k] }

func (v *fakeView) Thread(k state.ThreadKey) (*state.ThreadView, bool) {
	t, ok := v.threads[k]
	return t, ok
}

func (v *fakeView) AcceptsImages(k state.ThreadKey) (accepts, known bool) {
	accepts, known = v.images[k]
	return accepts, known
}

func (v *fakeView) openThread(active string) *state.ThreadView {
	tv := &state.ThreadView{
		Key: testThreadKey, Phase: client.PhaseConnected, Live: true, ActiveTurn: active,
		Thread: rpc.Thread{ID: testThread, Cwd: "/home/dev/project"},
	}
	v.threads[tv.Key] = tv
	return tv
}

type clipboardStub struct {
	content platform.ClipboardContent
}

func (c clipboardStub) ReadClipboard(context.Context, platform.PasteKind) (platform.ClipboardContent, error) {
	return c.content, nil
}

func (h *harness) withClipboard(content platform.ClipboardContent) *harness {
	h.env.Clipboard = func() platform.ClipboardReader { return clipboardStub{content: content} }
	return h
}

type harness struct {
	t       *testing.T
	dock    *Dock
	router  *input.Router
	view    *fakeView
	env     frame
	now     time.Time
	intents []state.Intent
	routes  []shell.Route
	nodes   []input.SemanticNode
	ax      *accesstest.Frame
	woke    chan struct{}
}

func newHarness(t *testing.T, host Host) *harness {
	t.Helper()
	shaper, err := platform.NewShaper()
	if err != nil {
		t.Fatal(err)
	}
	h := &harness{t: t, router: new(input.Router), view: newFakeView(), now: time.Unix(1_800_000_000, 0), woke: make(chan struct{}, 16)}
	host.Wake = func() { h.woke <- struct{}{} }
	h.dock, err = New(host)
	if err != nil {
		t.Fatal(err)
	}
	h.env = frame{
		Env: shell.Env{
			Theme: theme.Night, Shaper: shaper, Window: testWindow, Thread: testThreadKey,
			Submit: func(i state.Intent) { h.intents = append(h.intents, i) },
			Open:   func(r shell.Route) { h.routes = append(h.routes, r) },
		},
		view: h.view,
	}
	return h
}

func (h *harness) frame() {
	h.t.Helper()
	h.now = h.now.Add(16 * time.Millisecond)
	ops := new(op.Ops)
	gtx := layout.Context{
		Ops:         ops,
		Now:         h.now,
		Metric:      unit.Metric{PxPerDp: 1, PxPerSp: 1},
		Constraints: layout.Constraints{Max: image.Pt(900, 2000)},
		Source:      h.router.Source(),
	}
	gtx = h.ax.Begin(gtx)
	h.dock.render(gtx, h.env)
	h.router.Frame(ops)
	h.ax.End(h.t, gtx)
	h.nodes = h.router.AppendSemantics(h.nodes[:0])
}

func (h *harness) focus() {
	h.frame()
	h.router.Source().Execute(key.FocusCmd{Tag: h.dock.FocusTag()})
	h.frame()
	h.frame()
}

func (h *harness) wait(d time.Duration) {
	h.now = h.now.Add(d)
	h.frame()
}

func (h *harness) text() string { return h.dock.editor.Draft().Text }

func (h *harness) typeText(s string) {
	h.t.Helper()
	at := utf8.RuneCountInString(h.text())
	end := at + utf8.RuneCountInString(s)
	h.router.Queue(
		key.EditEvent{Range: key.Range{Start: at, End: at}, Text: s},
		key.SelectionEvent{Start: end, End: end},
	)
	h.frame()
	h.frame()
}

func (h *harness) press(name key.Name, mods key.Modifiers) {
	h.t.Helper()
	h.router.Queue(key.Event{Name: name, Modifiers: mods, State: key.Press})
	h.frame()
	h.frame()
}

func (h *harness) awaitWake() {
	h.t.Helper()
	select {
	case <-h.woke:
	case <-time.After(5 * time.Second):
		h.t.Fatal("the asynchronous attachment never arrived")
	}
	h.frame()
	h.frame()
}

func (h *harness) node(label string) (input.SemanticNode, bool) {
	for _, n := range h.nodes {
		if n.Desc.Label == label {
			return n, true
		}
	}
	return input.SemanticNode{}, false
}

func (h *harness) shows(label string) bool {
	_, ok := h.node(label)
	return ok
}

func (h *harness) showsPrefix(prefix string) bool {
	for _, n := range h.nodes {
		if strings.HasPrefix(n.Desc.Label, prefix) {
			return true
		}
	}
	return false
}

func (h *harness) click(label string) {
	h.t.Helper()
	var b image.Rectangle
	for _, n := range h.nodes {
		if n.Desc.Class == semantic.Button && n.Desc.Label == label {
			b = n.Desc.Bounds
			break
		}
	}
	if b.Empty() {
		h.t.Fatalf("no button named %q", label)
	}
	at := f32.Pt(float32(b.Min.X+b.Max.X)/2, float32(b.Min.Y+b.Max.Y)/2)
	h.router.Queue(
		pointer.Event{Kind: pointer.Press, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at},
		pointer.Event{Kind: pointer.Release, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at},
	)
	h.frame()
	h.frame()
}

func intentsOf[T state.Intent](h *harness) []T {
	var out []T
	for _, i := range h.intents {
		if v, ok := i.(T); ok {
			out = append(out, v)
		}
	}
	return out
}

func lastPut(h *harness) state.Draft {
	h.t.Helper()
	puts := intentsOf[state.PutDraft](h)
	if len(puts) == 0 {
		h.t.Fatal("no draft was stored")
	}
	return puts[len(puts)-1].Draft
}
