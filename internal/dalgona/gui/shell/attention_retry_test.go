//go:build gui

package shell

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func doneCount(n *fakeNotifier) int {
	n.mu.Lock()
	defer n.mu.Unlock()
	got := 0
	for _, sent := range n.sent {
		if sent.Title == "Done" {
			got++
		}
	}
	return got
}

func TestDroppedNotificationRetriesOnNextPass(t *testing.T) {
	n := &fakeNotifier{permission: platform.PermissionGranted}
	a := NewAttention(n, true, nil, nil)
	key := state.ThreadKey{Endpoint: remote, Thread: "t-9"}
	hidden := func(state.ThreadKey) bool { return false }
	for range sendQueue {
		if !a.queue(attentionJob{note: platform.Notification{Title: "filler"}}) {
			t.Fatal("could not saturate the notification queue")
		}
	}
	a.Observe(turnView(key, "inProgress"), hidden)
	a.Observe(turnView(key, "completed"), hidden)
	drain(a)
	if got := doneCount(n); got != 0 {
		t.Fatalf("a notification sent %d completions through a full queue, want 0", got)
	}
	a.Observe(turnView(key, "completed"), hidden)
	drain(a)
	if got := doneCount(n); got != 1 {
		t.Fatalf("completions sent after retry = %d, want 1", got)
	}
}

func TestAttentionStatePrunedForClosedThreads(t *testing.T) {
	n := &fakeNotifier{permission: platform.PermissionGranted}
	a := NewAttention(n, true, nil, nil)
	key := state.ThreadKey{Endpoint: remote, Thread: "t-9"}
	hidden := func(state.ThreadKey) bool { return false }
	a.Observe(turnView(key, "inProgress"), hidden)
	a.Observe(turnView(key, "completed"), hidden)
	drain(a)
	if got := doneCount(n); got != 1 {
		t.Fatalf("completions sent = %d, want 1", got)
	}
	a.Observe(state.Snapshot{}.View(), hidden)
	if len(a.seen) != 0 || len(a.running) != 0 {
		t.Fatalf("stale attention state survives a view without the thread: seen=%d running=%d", len(a.seen), len(a.running))
	}
}
