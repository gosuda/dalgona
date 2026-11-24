//go:build gui

package panel

import (
	"image"
	"testing"

	"gioui.org/io/input"
	"gioui.org/io/key"
	"gioui.org/io/semantic"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func compactHarness(t *testing.T) *harness {
	t.Helper()
	h := newHarness(t)
	h.thread = threadA
	h.snap.Threads = []*state.ThreadView{idleThread(threadA, "Parser work")}
	h.open(shell.Route{Panel: "compact", Thread: threadA})
	return h
}

func TestTabAndShiftTabStayInsideThePanel(t *testing.T) {
	h := compactHarness(t)
	run, dismiss := &h.panels.compact.op.run.Clickable, &h.panels.frame.dismiss.Clickable
	if !h.focused(run) {
		t.Fatalf("opening did not focus the first panel control")
	}
	for i, want := range []any{dismiss, run, dismiss} {
		h.press(key.NameTab, 0)
		if h.focused(&h.outside) || !h.focused(want) {
			t.Fatalf("Tab %d moved focus outside the panel or out of order", i+1)
		}
	}
	h.press(key.NameTab, key.ModShift)
	if !h.focused(run) {
		t.Fatal("Shift+Tab did not move back to the previous panel control")
	}
}

func TestStepMovesFocusAlongTheRingInBothDirections(t *testing.T) {
	h := compactHarness(t)
	run, dismiss := &h.panels.compact.op.run.Clickable, &h.panels.frame.dismiss.Clickable
	for i, move := range []struct {
		step int
		want any
	}{{1, dismiss}, {1, run}, {-1, dismiss}, {-1, run}} {
		h.panels.frame.Step(h.gtx(new(op.Ops)), move.step)
		h.settle()
		if !h.focused(move.want) {
			t.Fatalf("Step %d (%+d) did not reach the neighbouring ring control", i+1, move.step)
		}
	}
}

func TestEscapeClosesThePanelAndStopsWatching(t *testing.T) {
	h := compactHarness(t)
	h.click("Compact")
	call := h.call("thread/compact/start")
	h.running(call)
	h.take()
	h.press(key.NameEscape, 0)
	if h.closed != 1 {
		t.Fatalf("Escape closed the panel %d times, want 1", h.closed)
	}
	cancels := submitted[state.CancelCall](h.take())
	if len(cancels) != 1 || cancels[0].ID != call.ID {
		t.Fatalf("closing released %v, want the compaction call %s", cancels, call.ID)
	}
}

func TestBusyControlShowsProgressAndRejectsDuplicates(t *testing.T) {
	h := compactHarness(t)
	h.click("Compact")
	call := h.call("thread/compact/start")
	h.running(call)
	h.settle()
	if _, ok := h.button("Compacting"); !ok {
		t.Fatalf("busy control shows no progress: %q", h.labels())
	}
	h.click("Compacting")
	if n := len(submitted[state.Call](h.intents)); n != 1 {
		t.Fatalf("busy activation sent %d compaction calls, want 1", n)
	}
	if _, ok := h.button("Cancel"); !ok {
		t.Fatal("busy panel lost its Cancel action")
	}
	h.want("Closing stops watching; the daemon finishes the operation.")
}

func TestTitleAndFooterStayReachableWhenTheBodyOverflows(t *testing.T) {
	h := newHarness(t)
	h.size = image.Pt(320, 320)
	h.thread = threadA
	h.snap.Threads = []*state.ThreadView{idleThread(threadA, "Parser work")}
	h.open(shell.Route{Panel: "fork", Thread: threadA})
	turns := make([]rpc.TreeTurn, 60)
	for i := range turns {
		turns[i] = rpc.TreeTurn{TurnID: "t" + string(rune('A'+i%26)) + string(rune('a'+i/26)), Preview: "turn preview"}
	}
	h.answer(h.call("dalgona/thread/tree"), rpc.ThreadTreeResponse{Turns: turns})
	bounds := image.Rectangle{Max: h.size}
	title, ok := h.nodeLabel("Fork at a turn")
	footer, _ := h.button("Fork here")
	if !ok || !title.Desc.Bounds.Min.In(bounds) || title.Desc.Bounds.Min.Y >= footer.Desc.Bounds.Min.Y {
		t.Fatalf("title starts outside the window or below the footer: %v", title.Desc.Bounds)
	}
	for _, label := range []string{"Fork here", "Cancel"} {
		n, ok := h.button(label)
		if !ok || !n.Desc.Bounds.In(bounds) {
			t.Fatalf("footer control %q is unreachable at the minimum window: %v", label, n.Desc.Bounds)
		}
	}
}

func (h *harness) nodeLabel(label string) (input.SemanticNode, bool) {
	for _, node := range h.nodes {
		if node.Desc.Label == label {
			return node, true
		}
	}
	return input.SemanticNode{}, false
}

func TestRunningTurnRefusesGuardedPanelsWithInterruptAndClose(t *testing.T) {
	for _, panel := range []string{"new", "compact", "fork", "tree", "revert"} {
		h := newHarness(t)
		h.thread = threadA
		h.snap.Threads = []*state.ThreadView{runningThread(threadA, "Parser work", "turn-9")}
		h.open(shell.Route{Panel: panel, Thread: threadA})
		h.want("/"+panel+" is unavailable while a turn is working", "Turn turn-9 is working in Parser work")
		if calls := submitted[state.Call](h.take()); len(calls) != 0 {
			t.Fatalf("%s entered while a turn runs: %v", panel, calls)
		}
		h.click("Interrupt")
		interrupts := submitted[state.Interrupt](h.take())
		if len(interrupts) != 1 || interrupts[0] != (state.Interrupt{Window: window, Thread: threadA, Turn: "turn-9"}) {
			t.Fatalf("%s interrupt %v did not target turn-9 of thread-a", panel, interrupts)
		}
		h.click("Interrupting")
		if again := submitted[state.Interrupt](h.take()); len(again) != 0 {
			t.Fatalf("%s busy Interrupt sent again: %v", panel, again)
		}
		if _, ok := h.button("Close"); !ok {
			t.Fatalf("%s refusal has no Close", panel)
		}
	}
}

func TestInterruptedRefusalNeedsANewInvocation(t *testing.T) {
	h := newHarness(t)
	h.thread = threadA
	h.snap.Threads = []*state.ThreadView{runningThread(threadA, "Parser work", "turn-9")}
	h.open(shell.Route{Panel: "compact", Thread: threadA})
	h.click("Interrupt")
	h.snap.Threads = []*state.ThreadView{idleThread(threadA, "Parser work")}
	h.settle()
	h.want("The turn has stopped. Choose /compact again to continue.")
	if _, ok := h.button("Compact"); ok {
		t.Fatal("the refusal turned into the compaction panel without a new invocation")
	}
	if calls := submitted[state.Call](h.take()); len(calls) != 0 {
		t.Fatalf("interruption started %v", calls)
	}
}

func TestPendingActionKeepsTheThreadItWasOpenedFor(t *testing.T) {
	h := compactHarness(t)
	h.thread = threadB
	h.snap.Threads = append(h.snap.Threads, idleThread(threadB, "Other"))
	h.settle()
	h.click("Compact")
	call := h.call("thread/compact/start")
	if call.Endpoint != threadA.Endpoint || params[rpc.ThreadCompactStartParams](t, call).ThreadID != threadA.Thread {
		t.Fatalf("compaction went to %s %v after the window changed threads", call.Endpoint, call.Params)
	}
}

func TestAClosedPanelResultDoesNotReachTheNextPanel(t *testing.T) {
	h := compactHarness(t)
	h.click("Compact")
	first := h.call("thread/compact/start")
	h.open(shell.Route{Panel: "compact", Thread: threadA})
	h.fail(first, "late failure")
	h.reject("late failure")
	if _, ok := h.node(semantic.Button, "Compact"); !ok {
		t.Fatal("the reopened panel is not idle")
	}
}
