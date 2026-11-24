//go:build linux

package platform

import (
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/godbus/dbus/v5"
)

type notifyCall struct {
	app      string
	replaces uint32
	summary  string
	body     string
	actions  []string
}

type fakeNotificationServer struct {
	conn  *dbus.Conn
	mu    sync.Mutex
	next  uint32
	calls []notifyCall
}

func (f *fakeNotificationServer) GetCapabilities() ([]string, *dbus.Error) {
	return []string{"actions", "body", "body-markup"}, nil
}

func (f *fakeNotificationServer) Notify(app string, replaces uint32, _ string, summary, body string, actions []string, _ map[string]dbus.Variant, _ int32) (uint32, *dbus.Error) { //nolint:revive // specified D-Bus Notify method signature
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, notifyCall{app: app, replaces: replaces, summary: summary, body: body, actions: actions})
	if replaces != 0 {
		return replaces, nil
	}
	f.next++
	return f.next, nil
}

func (f *fakeNotificationServer) emit(t *testing.T, member string, values ...any) {
	t.Helper()
	if err := f.conn.Emit(notificationsObject, notificationsInterface+"."+member, values...); err != nil {
		t.Fatal(err)
	}
}

func (f *fakeNotificationServer) call(i int) notifyCall {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.calls[i]
}

func serveNotifications(t *testing.T) *fakeNotificationServer {
	t.Helper()
	address := startPortalTestBus(t)
	t.Setenv("DBUS_SESSION_BUS_ADDRESS", address)
	f := &fakeNotificationServer{conn: connectPortalTestBus(t, address)}
	if err := f.conn.Export(f, notificationsObject, notificationsInterface); err != nil {
		t.Fatal(err)
	}
	if _, err := f.conn.RequestName(notificationsService, dbus.NameFlagDoNotQueue); err != nil {
		t.Fatal(err)
	}
	return f
}

func awaitActivation(t *testing.T, n *Notifier) (string, bool) {
	t.Helper()
	select {
	case tag := <-n.Activations():
		return tag, true
	case <-time.After(500 * time.Millisecond):
		return "", false
	}
}

func TestNotifierReportsTheClickedNotificationTag(t *testing.T) {
	server := serveNotifications(t)
	var woken atomic.Int32
	n := NewNotifier(func() { woken.Add(1) })
	t.Cleanup(func() { _ = n.Close() })
	if p, err := n.Refresh(t.Context()); err != nil || p != PermissionGranted {
		t.Fatalf("refresh: %d, %v", p, err)
	}
	waiting := Notification{Title: "Waiting for your reply", Body: "<b>Fix</b> & ship", Tag: "local\x1fthr_1"}
	if err := n.Send(t.Context(), waiting); err != nil {
		t.Fatal(err)
	}
	if err := n.Send(t.Context(), Notification{Title: "Done", Body: "Other", Tag: "local\x1fthr_2"}); err != nil {
		t.Fatal(err)
	}
	checkNotificationPayload(t, server.call(0), waiting.Title)
	server.emit(t, "ActionInvoked", uint32(2), "other")
	if tag, ok := awaitActivation(t, n); ok {
		t.Fatalf("a non-default action activated %q", tag)
	}
	server.emit(t, "ActionInvoked", uint32(1), "default")
	tag, ok := awaitActivation(t, n)
	if !ok || tag != waiting.Tag {
		t.Fatalf("activation %q %v, want the first tag", tag, ok)
	}
	if n := woken.Load(); n != 1 {
		t.Fatalf("woken %d, want one wake", n)
	}
}

func checkNotificationPayload(t *testing.T, first notifyCall, title string) {
	t.Helper()
	if first.app != "dalgona" || first.replaces != 0 || first.summary != title || first.body != "&lt;b&gt;Fix&lt;/b&gt; &amp; ship" {
		t.Fatalf("first notification %+v", first)
	}
	if len(first.actions) != 2 || first.actions[0] != "default" {
		t.Fatalf("actions %q, want the default action first", first.actions)
	}
}

func TestNotifierReplacesANoticeWithTheSameTag(t *testing.T) {
	server := serveNotifications(t)
	n := NewNotifier(nil)
	t.Cleanup(func() { _ = n.Close() })
	if _, err := n.Refresh(t.Context()); err != nil {
		t.Fatal(err)
	}
	note := Notification{Title: "Waiting for your reply", Body: "thread", Tag: "t"}
	for range 2 {
		if err := n.Send(t.Context(), note); err != nil {
			t.Fatal(err)
		}
	}
	if got := server.call(1).replaces; got != 1 {
		t.Fatalf("second notice replaces %d, want 1", got)
	}
	server.emit(t, "NotificationClosed", uint32(1), uint32(2))
	server.emit(t, "ActionInvoked", uint32(1), "default")
	if tag, ok := awaitActivation(t, n); ok {
		t.Fatalf("a closed notice activated %q", tag)
	}
}

func TestNotifierWithoutAServiceIsUnavailable(t *testing.T) {
	address := startPortalTestBus(t)
	t.Setenv("DBUS_SESSION_BUS_ADDRESS", address)
	n := NewNotifier(nil)
	if err := n.Send(t.Context(), Notification{Title: "Done", Body: "x", Tag: "t"}); !errors.Is(err, ErrNotificationsOff) {
		t.Fatalf("send before permission: %v", err)
	}
	if p, err := n.Refresh(t.Context()); err == nil || p != PermissionUnavailable || n.Permission() != PermissionUnavailable {
		t.Fatalf("no service: %d, %v", p, err)
	}
}
