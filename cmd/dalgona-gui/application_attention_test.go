//go:build gui

package main

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type attentionRecorder struct {
	mu   sync.Mutex
	sent []platform.Notification
}

func (n *attentionRecorder) Permission() platform.NotificationPermission {
	return platform.PermissionGranted
}

func (n *attentionRecorder) Request(context.Context) (platform.NotificationPermission, error) {
	return platform.PermissionGranted, nil
}

func (n *attentionRecorder) Refresh(context.Context) (platform.NotificationPermission, error) {
	return platform.PermissionGranted, nil
}

func (n *attentionRecorder) Send(_ context.Context, note platform.Notification) error {
	n.mu.Lock()
	defer n.mu.Unlock()
	n.sent = append(n.sent, note)
	return nil
}

func (n *attentionRecorder) OpenSettings() error { return nil }

func (n *attentionRecorder) titles() []string {
	n.mu.Lock()
	defer n.mu.Unlock()
	var out []string
	for _, sent := range n.sent {
		out = append(out, sent.Title)
	}
	return out
}

func (n *attentionRecorder) bodies() []string {
	n.mu.Lock()
	defer n.mu.Unlock()
	var out []string
	for _, sent := range n.sent {
		out = append(out, sent.Body)
	}
	return out
}

func turnSnapshot(key state.ThreadKey, status string) *state.View {
	tv := &state.ThreadView{
		Key:    key,
		Live:   true,
		Thread: rpc.Thread{ID: key.Thread, Name: "Release"},
		Turns:  []state.TurnView{{ID: "t1", Status: status}},
	}
	return state.Snapshot{
		Endpoints: []state.EndpointView{{ID: key.Endpoint, Label: "Lab box", Kind: client.EndpointDaemon, Phase: client.PhaseConnected}},
		Threads:   []*state.ThreadView{tv},
	}.View()
}

func announcedApp(t *testing.T, n *attentionRecorder) *application {
	t.Helper()
	a := &application{
		attention: shell.NewAttention(n, true, nil, nil),
		presence:  map[state.WindowID]presence{},
	}
	ctx, stop := context.WithCancel(context.Background())
	t.Cleanup(stop)
	go a.attention.Run(ctx)
	return a
}

func waitTitles(t *testing.T, n *attentionRecorder, want int) []string {
	t.Helper()
	for range 200 {
		if got := n.titles(); len(got) >= want {
			return got
		}
		time.Sleep(25 * time.Millisecond)
	}
	return n.titles()
}

func TestAnnounceNotifiesWithoutDraw(t *testing.T) {
	n := &attentionRecorder{}
	a := announcedApp(t, n)
	key := state.ThreadKey{Endpoint: "lab", Thread: "t-9"}
	a.announceView(turnSnapshot(key, "inProgress"))
	a.announceView(turnSnapshot(key, "completed"))
	if got := waitTitles(t, n, 1); len(got) != 1 || got[0] != "Done" {
		t.Fatalf("announced %v, want one Done notification", got)
	}
	if got := n.bodies(); len(got) != 1 || got[0] != "Release · Lab box" {
		t.Fatalf("notification bodies %v, want the thread and endpoint", got)
	}
}

func TestAnnounceSkipsVisibleThread(t *testing.T) {
	n := &attentionRecorder{}
	a := announcedApp(t, n)
	key := state.ThreadKey{Endpoint: "lab", Thread: "t-9"}
	a.presence["window-1"] = presence{focused: true, open: key}
	a.announceView(turnSnapshot(key, "inProgress"))
	a.announceView(turnSnapshot(key, "completed"))
	time.Sleep(200 * time.Millisecond)
	if got := n.titles(); len(got) != 0 {
		t.Fatalf("announced %v for a visible thread", got)
	}
}

func TestConfigureUpdatesPresenceWithoutDraw(t *testing.T) {
	n := &attentionRecorder{}
	a := announcedApp(t, n)
	key := state.ThreadKey{Endpoint: "lab", Thread: "t-9"}
	win := &window{app: a, id: "window-1", frame: shell.New(shell.Config{Window: "window-1"})}
	win.frame.Adopt(key)
	win.configure(true)
	if !a.visible(key) {
		t.Fatal("a focused window showing the thread is not visible")
	}
	win.configure(false)
	if a.visible(key) {
		t.Fatal("an unfocused window still counts as visible")
	}
}

func themeSnapshot() *state.View {
	return state.Snapshot{Endpoints: []state.EndpointView{
		{ID: "a", Label: "A", Settings: []rpc.Setting{{Key: "theme", Value: "night"}}},
		{ID: "b", Label: "B", Settings: []rpc.Setting{{Key: "theme", Value: "day"}}},
	}}.View()
}

func readTheme(t *testing.T, dir string) string {
	t.Helper()
	for range 200 {
		if got := readPreference(dir, themeFile); got != "" {
			return got
		}
		time.Sleep(25 * time.Millisecond)
	}
	return ""
}

func TestThemePersistsOnlyRecentWindowSetting(t *testing.T) {
	dir := t.TempDir()
	a := &application{theme: loadPreference(dir, themeFile), recent: "window-1"}
	v := themeSnapshot()
	other := &window{app: a, id: "window-2", frame: shell.New(shell.Config{Window: "window-2"})}
	self := &window{app: a, id: "window-1", frame: shell.New(shell.Config{Window: "window-1"})}
	a.themeFor(v, other, look{})
	time.Sleep(200 * time.Millisecond)
	if got := readPreference(dir, themeFile); got != "" {
		t.Fatalf("a background window persisted theme %q", got)
	}
	a.themeFor(v, self, look{})
	if got := readTheme(t, dir); got != "night" {
		t.Fatalf("the recent window persisted theme %q, want night", got)
	}
}
