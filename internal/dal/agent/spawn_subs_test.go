package agent

import (
	"testing"
	"time"
)

func markBusy(t *testing.T, thread *Thread) {
	t.Helper()
	if _, err := ask(thread, func(a *actor) bool {
		a.busy = true
		return true
	}); err != nil {
		t.Fatalf("agent: set busy: %v", err)
	}
}

func clearBusy(t *testing.T, thread *Thread) {
	t.Helper()
	if _, err := ask(thread, func(a *actor) bool {
		a.busy = false
		return true
	}); err != nil {
		t.Fatalf("agent: clear busy: %v", err)
	}
}

func waitSubscribers(t *testing.T, thread *Thread, want int) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for {
		status, err := thread.Status()
		if err != nil {
			t.Fatalf("agent: status: %v", err)
		}
		if status.Subscribers == want {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("agent: subscribers = %d, want %d", status.Subscribers, want)
		}
		time.Sleep(time.Millisecond)
	}
}

func overflowSubscribers(t *testing.T, thread *Thread) {
	t.Helper()
	started := make(chan struct{})
	release := make(chan struct{})
	blockErr := make(chan error, 1)
	go func() {
		blockErr <- thread.do(func(a *actor) {
			close(started)
			<-release
		})
	}()
	<-started
	floodErr := make(chan error, 1)
	go func() {
		floodErr <- thread.do(func(a *actor) {
			for range subscriberBuffer + 10 {
				a.publish(Update{Kind: UpdateTextDelta, Delta: "x"})
			}
		})
	}()
	close(release)
	if err := <-floodErr; err != nil {
		t.Fatalf("agent: flood: %v", err)
	}
	if err := <-blockErr; err != nil {
		t.Fatalf("agent: blocker: %v", err)
	}
}

func TestWaitTurnEndReleasesResubscribedChannel(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	child := h.start()
	markBusy(t, child)
	done := make(chan error, 1)
	go func() { done <- waitTurnEnd(t.Context(), child) }()
	waitSubscribers(t, child, 1)
	overflowSubscribers(t, child)
	waitSubscribers(t, child, 0)
	waitSubscribers(t, child, 1)
	clearBusy(t, child)
	deadline := time.Now().Add(5 * time.Second)
	for {
		select {
		case err := <-done:
			if err != nil {
				t.Fatalf("agent: waitTurnEnd: %v", err)
			}
			waitSubscribers(t, child, 0)
			return
		default:
		}
		if time.Now().After(deadline) {
			t.Fatal("agent: waitTurnEnd did not return")
		}
		child.publish(Update{Kind: UpdateTextDelta, Delta: "wake"})
		time.Sleep(time.Millisecond)
	}
}
