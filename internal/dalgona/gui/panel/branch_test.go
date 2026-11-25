//go:build gui

package panel

import (
	"slices"
	"testing"

	"gioui.org/io/key"
	"gioui.org/io/semantic"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

var sampleTurns = []rpc.TreeTurn{
	{TurnID: "t3", Preview: "third"},
	{TurnID: "t1", Preview: "", Active: true},
	{TurnID: "t2", ParentTurnID: "t1", Preview: "second", Active: true},
}

func threadPanel(t *testing.T, panel, turn string) *harness {
	t.Helper()
	h := newHarness(t)
	h.thread = threadA
	h.snap.Threads = []*state.ThreadView{idleThread(threadA, "Parser work")}
	h.open(shell.Route{Panel: panel, Thread: threadA, Turn: turn})
	return h
}

func (h *harness) loadTree(turns []rpc.TreeTurn) {
	h.t.Helper()
	call := h.call("dalgona/thread/tree")
	if p := params[rpc.ThreadTreeParams](h.t, call); call.Endpoint != threadA.Endpoint || p.ThreadID != threadA.Thread {
		h.t.Fatalf("tree read %s %v, want thread-a on local", call.Endpoint, p)
	}
	h.answer(call, rpc.ThreadTreeResponse{Turns: turns})
}

func (h *harness) radios() []string {
	var out []string
	for _, n := range h.nodes {
		if n.Desc.Class == semantic.RadioButton {
			out = append(out, n.Desc.Label)
		}
	}
	return out
}

func (h *harness) selected() string {
	for _, n := range h.nodes {
		if n.Desc.Class == semantic.RadioButton && n.Desc.Selected {
			return n.Desc.Label
		}
	}
	return ""
}

func TestTurnListKeepsServerOrderPreviewsAndActiveMarker(t *testing.T) {
	h := threadPanel(t, "fork", "")
	h.loadTree(sampleTurns)
	want := []string{"third", "Untitled turn Active", "second Active"}
	if got := h.radios(); !slices.Equal(got, want) {
		t.Fatalf("turn rows %q, want %q", got, want)
	}
}

func TestTurnListArrowKeysMoveTheSingleChoice(t *testing.T) {
	h := threadPanel(t, "fork", "")
	h.loadTree(sampleTurns)
	h.choose("third")
	h.press(key.NameDownArrow, 0)
	if got := h.selected(); got != "Untitled turn Active" {
		t.Fatalf("ArrowDown selected %q", got)
	}
	h.press(key.NameUpArrow, 0)
	if got := h.selected(); got != "third" {
		t.Fatalf("ArrowUp selected %q", got)
	}
}

func TestEmptyTurnListExplainsAndBlocksTheAction(t *testing.T) {
	h := threadPanel(t, "fork", "")
	h.loadTree(nil)
	h.want("This thread has no turns to fork from.")
	h.click("Fork here")
	if n := len(submitted[state.Call](h.intents)); n != 1 {
		t.Fatalf("an empty list still sent a fork: %v", h.methods())
	}
}

func TestConfirmationRevalidatesTheSelectedTurn(t *testing.T) {
	var l turnList
	l.reset("t1")
	if _, ok := l.confirm(sampleTurns); !ok {
		t.Fatal("a present turn failed revalidation")
	}
	if _, ok := l.confirm(sampleTurns[:1]); ok {
		t.Fatal("a vanished turn passed revalidation")
	}
	l.keep(sampleTurns[:1])
	if l.selected != "" {
		t.Fatalf("loading kept the vanished turn %q", l.selected)
	}
	l.reset("t3")
	l.keep(sampleTurns)
	if l.selected != "t3" {
		t.Fatal("loading dropped a turn that is still available")
	}
}

func TestCompactionExplainsTheJournalAndRefreshesOnSuccess(t *testing.T) {
	h := threadPanel(t, "compact", "")
	h.want("Compact this thread? Earlier turns stay in the journal.")
	if calls := submitted[state.Call](h.intents); len(calls) != 0 {
		t.Fatalf("opening compacted without the explicit action: %v", calls)
	}
	h.click("Compact")
	call := h.call("thread/compact/start")
	if !call.Operation || params[rpc.ThreadCompactStartParams](t, call).ThreadID != threadA.Thread {
		t.Fatalf("compaction call %+v", call)
	}
	h.answer(call, map[string]any{})
	if refresh := submitted[state.Refresh](h.intents); len(refresh) != 1 || refresh[0].Thread != threadA {
		t.Fatalf("success refreshed %v, want thread-a", refresh)
	}
	if h.closed != 1 {
		t.Fatal("successful compaction kept the panel open")
	}
}

func TestCompactionFailureStaysInThePanel(t *testing.T) {
	h := threadPanel(t, "compact", "")
	h.click("Compact")
	h.fail(h.call("thread/compact/start"), "thread is busy")
	h.want("thread is busy")
	if h.closed != 0 {
		t.Fatal("a failed compaction closed the panel")
	}
}

func TestForkSendsTheExactTurnAndOpensTheReturnedThread(t *testing.T) {
	h := threadPanel(t, "fork", "")
	h.loadTree(sampleTurns)
	h.choose("second Active")
	h.click("Fork here")
	call := h.call("thread/fork")
	if p := params[rpc.ThreadForkParams](t, call); !call.Operation || p.ThreadID != threadA.Thread || p.LastTurnID != "t2" {
		t.Fatalf("fork params %+v", p)
	}
	h.answer(call, rpc.ThreadStartResponse{Thread: rpc.Thread{ID: "fork-1"}})
	if len(h.navigated) != 1 || h.navigated[0] != (state.ThreadKey{Endpoint: local, Thread: "fork-1"}) {
		t.Fatalf("fork opened %v", h.navigated)
	}
}

func TestForkFailureRetainsTheTurnAndSource(t *testing.T) {
	h := threadPanel(t, "fork", "")
	h.loadTree(sampleTurns)
	h.choose("third")
	h.click("Fork here")
	h.fail(h.call("thread/fork"), "fork refused")
	h.want("fork refused", "Thread: Parser work on local")
	if got := h.selected(); got != "third" {
		t.Fatalf("failure changed the selection to %q", got)
	}
}

func TestTreeShowsParentsAndLeafAndSetsTheExactTurn(t *testing.T) {
	h := threadPanel(t, "tree", "")
	h.loadTree(sampleTurns)
	h.want("Follows Untitled turn · Current leaf", "workspace files stay as they are")
	if got := h.selected(); got != "second Active" {
		t.Fatalf("tree preselected %q, want the active leaf", got)
	}
	h.choose("third")
	h.click("Switch branch")
	call := h.call("dalgona/thread/leaf/set")
	if p := params[rpc.ThreadLeafSetParams](t, call); p.ThreadID != threadA.Thread || p.TurnID != "t3" {
		t.Fatalf("leaf params %+v", p)
	}
	h.fail(call, "leaf refused")
	if got := h.selected(); got != "third" || h.closed != 0 {
		t.Fatalf("failure lost the selection %q or closed", got)
	}
	h.click("Switch branch")
	h.answer(h.call("dalgona/thread/leaf/set"), map[string]any{})
	if refresh := submitted[state.Refresh](h.intents); len(refresh) != 1 {
		t.Fatal("a new leaf did not reload the active path")
	}
}

func TestRewindPresetsTheUserTurnAndNeedsConfirmation(t *testing.T) {
	h := threadPanel(t, "revert", "t2")
	h.loadTree(sampleTurns)
	h.want("Rewind to before: second", "Files changed by later turns are not reverted.")
	if got := h.radios(); slices.Contains(got, "third") {
		t.Fatalf("rewind offered a turn off the active path: %q", got)
	}
	if calls := h.methods(); slices.Contains(calls, "thread/revert") {
		t.Fatal("rewind ran without confirmation")
	}
	h.click("Rewind")
	call := h.call("thread/revert")
	if p := params[rpc.ThreadRevertParams](t, call); p.ThreadID != threadA.Thread || p.BeforeTurnID != "t2" {
		t.Fatalf("revert params %+v", p)
	}
	h.fail(call, "not idle")
	h.want("not idle", "Rewind to before: second")
	if _, ok := h.button("Keep"); !ok {
		t.Fatal("rewind lost its Keep action")
	}
}
