//go:build gui

package shell

import (
	"testing"

	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func pendingView() *state.View {
	a := state.ThreadKey{Endpoint: local, Thread: "fix"}
	b := state.ThreadKey{Endpoint: remote, Thread: "plan"}
	fix := openThread(a, "Fix the parser")
	fix.Requests = []state.RequestView{{Key: state.RequestKey{Thread: a, Request: "r1"}, Method: "item/commandExecution/requestApproval", Actionable: true}}
	plan := openThread(b, "Plan the release")
	plan.Requests = []state.RequestView{
		{Key: state.RequestKey{Thread: b, Request: "r2"}, Method: "item/tool/requestUserInput", Actionable: true},
		{Key: state.RequestKey{Thread: b, Request: "r3"}, Method: "item/fileChange/requestApproval", Answered: true},
	}
	snap := state.Snapshot{
		Endpoints: []state.EndpointView{endpoint(local, "local", client.PhaseConnected), endpoint(remote, "lab", client.PhaseConnected)},
		Lanes:     map[client.EndpointID]state.LaneView{local: loaded(), remote: loaded()},
		Threads:   []*state.ThreadView{fix, plan},
	}
	return snap.View()
}

func TestCloseExplainsPendingDecisionsByEndpoint(t *testing.T) {
	h := newHarness(t)
	v := pendingView()
	h.settle(v)
	if !h.shell.Closing(v, CloseScope{}) {
		t.Fatal("close with pending decisions proceeded without asking")
	}
	h.frame(v)
	for _, want := range []string{"On local: Approval in Fix the parser", "On lab: Question in Plan the release", "do not stay open"} {
		if !h.has(want) {
			t.Errorf("close panel lacks %q: %v", want, h.labels())
		}
	}
}

func TestCloseSkipsDecisionsAnotherWindowKeeps(t *testing.T) {
	h := newHarness(t)
	v := pendingView()
	elsewhere := func(k state.ThreadKey) bool { return true }
	if h.shell.Closing(v, CloseScope{Elsewhere: elsewhere}) {
		t.Fatal("asked although another window keeps every pending thread")
	}
}

func TestCloseAnywayConfirmsAndKeepOpenAsksAgain(t *testing.T) {
	h := newHarness(t)
	v := pendingView()
	h.settle(v)
	h.shell.Closing(v, CloseScope{})
	h.frame(v)
	h.click(v, keepOpenLabel)
	if h.has("Close with pending decisions?") {
		t.Fatal("Keep open left the panel up")
	}
	if !h.shell.Closing(v, CloseScope{}) {
		t.Fatal("a second close did not ask again")
	}
	h.frame(v)
	n, ok := h.button(closeAnywayLabel)
	if !ok {
		t.Fatal("no Close anyway")
	}
	h.clickAt(n.Desc.Bounds)
	if !h.frame(v) {
		t.Fatal("Close anyway did not confirm")
	}
	if h.shell.Closing(v, CloseScope{}) {
		t.Fatal("a confirmed close asked again")
	}
}

func TestEscapeKeepsTheWindowOpen(t *testing.T) {
	h := newHarness(t)
	v := pendingView()
	h.settle(v)
	h.shell.Closing(v, CloseScope{})
	h.settle(v)
	h.press(v, key.NameEscape, 0)
	h.frame(v)
	if h.has("Close with pending decisions?") {
		t.Fatal("Escape left the close panel open")
	}
}

func draftView(queued bool) *state.View {
	a := state.ThreadKey{Endpoint: local, Thread: "fix"}
	b := state.ThreadKey{Endpoint: remote, Thread: "plan"}
	typed := state.DraftKey{Endpoint: local, Window: testWindow, Thread: "fix"}
	waiting := state.DraftKey{Endpoint: remote, Window: testWindow, Thread: "plan"}
	elsewhere := state.DraftKey{Endpoint: local, Window: "window-2", Thread: "plan"}
	snap := state.Snapshot{
		Endpoints: []state.EndpointView{endpoint(local, "local", client.PhaseConnected), endpoint(remote, "lab", client.PhaseConnected)},
		Lanes:     map[client.EndpointID]state.LaneView{local: loaded(), remote: loaded()},
		Threads:   []*state.ThreadView{openThread(a, "Fix the parser"), openThread(b, "Plan the release")},
		Drafts:    map[state.DraftKey]state.Draft{typed: {Text: "half a thought"}, elsewhere: {Text: "another window"}},
		FollowUps: map[state.DraftKey][]state.FollowUp{},
	}
	if queued {
		snap.FollowUps[waiting] = []state.FollowUp{{ID: 1, Text: "next"}, {ID: 2, Text: "after"}}
	}
	return snap.View()
}

func TestClosingOneWindowNamesTheDraftsAndFollowUpsItDiscards(t *testing.T) {
	h := newHarness(t)
	v := draftView(true)
	h.settle(v)
	if !h.shell.Closing(v, CloseScope{}) {
		t.Fatal("closing a window with unsent text proceeded without asking")
	}
	h.frame(v)
	for _, want := range []string{unsentTitle, "On local: unsent draft in Fix the parser", "On lab: 2 queued follow-ups in Plan the release", "Other windows keep their own"} {
		if !h.has(want) {
			t.Errorf("close panel lacks %q: %v", want, h.labels())
		}
	}
	if h.has("Plan the release; ") || h.has(decisionsTitle) {
		t.Errorf("close panel names text it keeps: %v", h.labels())
	}
}

func TestClosingTheLastWindowNamesOnlyTextThatWillNotSurvive(t *testing.T) {
	h := newHarness(t)
	v := draftView(true)
	h.settle(v)
	if !h.shell.Closing(v, CloseScope{Final: true}) {
		t.Fatal("queued follow-ups do not survive an exit, but the close did not ask")
	}
	h.frame(v)
	if h.has("unsent draft") {
		t.Fatalf("a saved draft was named as lost: %v", h.labels())
	}
	if !h.has("On lab: 2 queued follow-ups in Plan the release") || !h.has("do not survive closing dalgona") {
		t.Fatalf("close panel lacks the lost follow-ups: %v", h.labels())
	}
}

func TestClosingTheLastWindowWithSavedDraftsDoesNotAsk(t *testing.T) {
	h := newHarness(t)
	if h.shell.Closing(draftView(false), CloseScope{Final: true}) {
		t.Fatal("an exit asked although every draft is saved for restore")
	}
}
