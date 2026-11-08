package agent

import (
	"context"
	"errors"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/read"
)

const wantAdvisory = `<advisory advisor="advisor" severity="blocker">run the tests &lt;now&gt;</advisory>`

func blocker(delivery Delivery, review uint64) Note {
	return Note{Advisor: "advisor", Severity: "blocker", Text: "run the tests <now>", ReviewSeq: review, Delivery: delivery}
}

func countNotes(events []event.Seqd) int {
	count := 0
	for _, item := range events {
		if _, ok := item.Event.(event.AdvisorNote); ok {
			count++
		}
	}
	return count
}

func TestSteerNoteReachesNextRequest(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	gate := make(chan struct{})
	h.model.push(response{body: sseText("working"), gate: gate}, text("tests ran"))
	thread := h.start()
	h.submit(thread, "do it")
	h.waitRequests(1)
	delivery, err := thread.Deliver(t.Context(), blocker(DeliverySteer, 1))
	if err != nil || delivery != DeliverySteer {
		t.Fatalf("delivery = %q, %v", delivery, err)
	}
	close(gate)
	h.waitIdle(thread)
	if h.model.count() != 2 {
		t.Fatalf("requests = %d", h.model.count())
	}
	if last := lastText(t, userTexts(h.model.request(1))); last != wantAdvisory {
		t.Fatalf("last user text = %q", last)
	}
	again, err := thread.Deliver(t.Context(), blocker(DeliveryAside, 1))
	if err != nil || again != DeliverySteer || countNotes(h.events(thread.ID())) != 1 {
		t.Fatalf("redelivery = %q, %v, notes = %d", again, err, countNotes(h.events(thread.ID())))
	}
}

type boundaryLog struct {
	mu    sync.Mutex
	items []Boundary
}

func (l *boundaryLog) hook(b Boundary) Wait {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.items = append(l.items, b)
	return nil
}

func (l *boundaryLog) last() Boundary {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.items[len(l.items)-1]
}

func (h *harness) idleThread(prompt string) *Thread {
	h.t.Helper()
	thread := h.start()
	h.submit(thread, prompt)
	h.waitIdle(thread)
	return thread
}

func TestContinueStartsOneContinuationTurn(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.idleThread("hello")
	log := &boundaryLog{}
	if err := thread.SetBoundaryHook(log.hook); err != nil {
		t.Fatal(err)
	}
	delivery, err := thread.Deliver(t.Context(), blocker(DeliveryContinue, 1))
	if err != nil || delivery != DeliveryContinue {
		t.Fatalf("delivery = %q, %v", delivery, err)
	}
	h.waitIdle(thread)
	if last := lastText(t, userTexts(h.model.request(1))); last != wantAdvisory {
		t.Fatalf("continuation text = %q", last)
	}
	if last := log.last(); last.Kind != BoundaryAgentEnd || !last.Continuation {
		t.Fatalf("last boundary = %+v", last)
	}
}

func TestInterruptTurnsContinueIntoAside(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.idleThread("hello")
	thread.Interrupt()
	delivery, err := thread.Deliver(t.Context(), blocker(DeliveryContinue, 2))
	if err != nil || delivery != DeliveryAside || h.model.count() != 1 {
		t.Fatalf("interrupted delivery = %q, %v, requests = %d", delivery, err, h.model.count())
	}
	h.submit(thread, "next")
	h.waitIdle(thread)
	if last := lastText(t, userTexts(h.model.request(1))); !strings.HasPrefix(last, "next") || !strings.HasSuffix(last, wantAdvisory) {
		t.Fatalf("aside not attached to the next prompt: %q", last)
	}
	if status, err := thread.Status(); err != nil || !status.AutoContinue {
		t.Fatalf("user input must restore auto-continue: %+v, %v", status, err)
	}
}

func TestAgentEndWaitHoldsTurnUntilReleased(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	release := make(chan struct{})
	reached := make(chan struct{}, 4)
	if err := thread.SetBoundaryHook(func(b Boundary) Wait {
		if b.Kind != BoundaryAgentEnd {
			return nil
		}
		reached <- struct{}{}
		return func(ctx context.Context) error {
			select {
			case <-release:
				return nil
			case <-ctx.Done():
				return ctx.Err()
			}
		}
	}); err != nil {
		t.Fatal(err)
	}
	h.submit(thread, "hello")
	<-reached
	if _, err := thread.Deliver(t.Context(), blocker(DeliverySteer, 1)); err != nil {
		t.Fatal(err)
	}
	time.Sleep(50 * time.Millisecond)
	if types := h.types(thread.ID()); types[len(types)-1] == "turn.completed" {
		t.Fatalf("turn completed during the wait: %v", types)
	}
	close(release)
	h.waitIdle(thread)
	if h.model.count() != 2 || lastTurn(h.events(thread.ID())).Status != statusCompleted {
		t.Fatalf("requests = %d, events = %v", h.model.count(), h.types(thread.ID()))
	}
}

func mustRead(t *testing.T, thread *Thread, after uint64) Read {
	t.Helper()
	read, err := thread.ReadAfter(after)
	if err != nil {
		t.Fatal(err)
	}
	return read
}

func TestReadAfterReportsResetOnRevert(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.idleThread("one")
	first := mustRead(t, thread, 0)
	if first.Reset || len(first.Events) == 0 {
		t.Fatalf("first read = %+v", first)
	}
	h.submit(thread, "two")
	h.waitIdle(thread)
	delta := mustRead(t, thread, first.Head)
	if delta.Reset || len(delta.Events) == 0 || delta.Events[0].Seq != first.Head+1 {
		t.Fatalf("delta = %+v", delta)
	}
	if err := thread.Revert(lastTurn(h.events(thread.ID())).TurnID); err != nil {
		t.Fatal(err)
	}
	if reset := mustRead(t, thread, delta.Head); !reset.Reset || len(reset.Events) != len(first.Events) {
		t.Fatalf("reset = %+v (first had %d events)", reset, len(first.Events))
	}
}

func TestAdvisorChildSessionsStayHidden(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	parent := h.start()
	if _, err := h.host.Start(t.Context(), ThreadSpec{Parent: parent.ID(), Advisor: "advisor"}); !errors.Is(err, ErrNeedsOverrides) {
		t.Fatalf("advisor without tools = %v", err)
	}
	child, err := h.host.Start(t.Context(), ThreadSpec{
		Parent: parent.ID(), Advisor: "advisor", Tools: []tool.Tool{read.New()}, System: "advisor prompt",
	})
	if err != nil {
		t.Fatal(err)
	}
	threads := h.host.Threads()
	if len(threads) != 1 || threads[0].ID != parent.ID() {
		t.Fatalf("threads = %+v", threads)
	}
	if id, ok := h.host.AdvisorChild(parent.ID(), "advisor"); !ok || id != child.ID() {
		t.Fatalf("advisor child = %q, %v", id, ok)
	}
	reopened := h.newHost()
	if _, err := reopened.Resume(t.Context(), child.ID()); !errors.Is(err, ErrNeedsOverrides) {
		t.Fatalf("resume without overrides = %v", err)
	}
	h.submit(child, "review")
	h.waitIdle(child)
	if got := h.model.request(0)["instructions"]; got != "advisor prompt" {
		t.Fatalf("child system prompt = %v", got)
	}
}

func TestAdvisorNoteRenderEscapes(t *testing.T) {
	t.Parallel()
	note := event.AdvisorNote{Advisor: `a"b`, Severity: "nit", Note: `x < y & "z"`}
	want := `<advisory advisor="a&#34;b" severity="nit">x &lt; y &amp; &#34;z&#34;</advisory>`
	if got := note.Render(); got != want {
		t.Fatalf("render = %q", got)
	}
}

func TestInterruptedTurnAnnouncesItsEnd(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	gate := make(chan struct{})
	t.Cleanup(func() { close(gate) })
	h.model.push(response{body: sseText("never"), gate: gate})
	thread := h.start()
	log := &boundaryLog{}
	if err := thread.SetBoundaryHook(log.hook); err != nil {
		t.Fatal(err)
	}
	h.submit(thread, "slow")
	h.waitRequests(1)
	thread.Interrupt()
	for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); time.Sleep(5 * time.Millisecond) {
		if ended, ok := log.ended(); ok {
			if ended.Kind != BoundaryAgentEnd || ended.Ended != statusInterrupted {
				t.Fatalf("end boundary = %+v", ended)
			}
			return
		}
	}
	t.Fatal("no end boundary for the interrupted turn")
}

func (l *boundaryLog) ended() (Boundary, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	for _, item := range l.items {
		if item.Ended != "" {
			return item, true
		}
	}
	return Boundary{}, false
}
