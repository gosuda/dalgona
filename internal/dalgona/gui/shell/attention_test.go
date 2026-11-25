//go:build gui

package shell

import (
	"context"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type fakeNotifier struct {
	mu         sync.Mutex
	permission platform.NotificationPermission
	sent       []platform.Notification
	requests   int
}

func (f *fakeNotifier) Permission() platform.NotificationPermission { return f.permission }

func (f *fakeNotifier) Request(context.Context) (platform.NotificationPermission, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.requests++
	f.permission = platform.PermissionGranted
	return f.permission, nil
}

func (f *fakeNotifier) Refresh(context.Context) (platform.NotificationPermission, error) {
	return f.permission, nil
}

func (f *fakeNotifier) Send(_ context.Context, n platform.Notification) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.sent = append(f.sent, n)
	return nil
}

func (f *fakeNotifier) OpenSettings() error { return nil }

func drain(a *Attention) {
	for {
		select {
		case job := <-a.jobs:
			a.perform(context.Background(), job)
		default:
			return
		}
	}
}

func turnView(key state.ThreadKey, status string) *state.View {
	tv := openThread(key, "Release")
	tv.Turns = []state.TurnView{{ID: "t1", Status: status}}
	return state.Snapshot{Endpoints: []state.EndpointView{endpoint(remote, "Lab box", client.PhaseConnected)}, Threads: []*state.ThreadView{tv}}.View()
}

func TestCompletionNotifiesOnceForATurnSeenRunning(t *testing.T) {
	n := &fakeNotifier{permission: platform.PermissionGranted}
	a := NewAttention(n, true, nil, nil)
	key := state.ThreadKey{Endpoint: remote, Thread: "t-9"}
	hidden := func(state.ThreadKey) bool { return false }
	a.Observe(turnView(key, "inProgress"), hidden)
	a.Observe(turnView(key, "completed"), hidden)
	a.Observe(turnView(key, "completed"), hidden)
	drain(a)
	if len(n.sent) != 1 {
		t.Fatalf("sent %d notifications, want 1: %v", len(n.sent), n.sent)
	}
	got := n.sent[0]
	if got.Title != "Done" || got.Body != "Release · Lab box" {
		t.Errorf("notification %+v, want Done naming the thread and endpoint", got)
	}
	if parsed, ok := ParseTag(got.Tag); !ok || parsed != key {
		t.Errorf("tag %q parses to %v, want %v", got.Tag, parsed, key)
	}
}

func TestHistoryAndVisibleThreadsDoNotNotify(t *testing.T) {
	n := &fakeNotifier{permission: platform.PermissionGranted}
	a := NewAttention(n, true, nil, nil)
	key := state.ThreadKey{Endpoint: remote, Thread: "t-9"}
	a.Observe(turnView(key, "failed"), func(state.ThreadKey) bool { return false })
	other := state.ThreadKey{Endpoint: remote, Thread: "t-10"}
	a.Observe(turnView(other, "inProgress"), nil)
	a.Observe(turnView(other, "interrupted"), func(k state.ThreadKey) bool { return k == other })
	drain(a)
	if len(n.sent) != 0 {
		t.Fatalf("sent %v for history or a visible thread", n.sent)
	}
}

func TestWaitingRequestAsksForAReply(t *testing.T) {
	n := &fakeNotifier{permission: platform.PermissionGranted}
	a := NewAttention(n, true, nil, nil)
	key := state.ThreadKey{Endpoint: remote, Thread: "t-9"}
	tv := openThread(key, "Release")
	tv.Requests = []state.RequestView{{Key: state.RequestKey{Thread: key, Request: "r1"}, Method: "item/commandExecution/requestApproval", Actionable: true}}
	v := state.Snapshot{Endpoints: []state.EndpointView{endpoint(remote, "lab", client.PhaseConnected)}, Threads: []*state.ThreadView{tv}}.View()
	a.Observe(v, nil)
	a.Observe(v, nil)
	drain(a)
	if len(n.sent) != 1 || n.sent[0].Title != "Waiting for your reply" {
		t.Fatalf("sent %v, want one Waiting for your reply", n.sent)
	}
}

func TestNotificationsNeedOptInAndPermission(t *testing.T) {
	key := state.ThreadKey{Endpoint: remote, Thread: "t-9"}
	for _, c := range []struct {
		name       string
		permission platform.NotificationPermission
		enabled    bool
	}{
		{"opted out", platform.PermissionGranted, false},
		{"undecided", platform.PermissionUndecided, true},
		{"denied", platform.PermissionDenied, true},
	} {
		n := &fakeNotifier{permission: c.permission}
		a := NewAttention(n, c.enabled, nil, nil)
		a.Observe(turnView(key, "inProgress"), nil)
		a.Observe(turnView(key, "completed"), nil)
		drain(a)
		if len(n.sent) != 0 || n.requests != 0 {
			t.Errorf("%s: sent %v and requested permission %d times", c.name, n.sent, n.requests)
		}
	}
}

func TestSettingsRowStatesAndAllow(t *testing.T) {
	cases := map[platform.NotificationPermission]string{
		platform.PermissionUndecided:   "Desktop notifications are off",
		platform.PermissionDenied:      "Desktop notifications are blocked in system settings",
		platform.PermissionUnavailable: "Desktop notifications are unavailable on this system",
	}
	for permission, want := range cases {
		if got := NewAttention(&fakeNotifier{permission: permission}, false, nil, nil).row().text; got != want {
			t.Errorf("permission %v row %q, want %q", permission, got, want)
		}
	}
	if got := NewAttention(nil, true, nil, nil).row().text; got != cases[platform.PermissionUnavailable] {
		t.Errorf("no notifier row %q", got)
	}
	n := &fakeNotifier{permission: platform.PermissionUndecided}
	var saved []bool
	a := NewAttention(n, false, func(on bool) { saved = append(saved, on) }, nil)
	if !a.queue(attentionJob{request: true}) {
		t.Fatal("could not queue the permission request")
	}
	drain(a)
	if n.requests != 1 || a.row().text != "Desktop notifications are on" || len(saved) != 1 || !saved[0] {
		t.Fatalf("after Allow: requests=%d row=%q saved=%v", n.requests, a.row().text, saved)
	}
}
