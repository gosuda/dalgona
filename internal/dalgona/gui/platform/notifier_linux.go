//go:build linux

package platform

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"slices"
	"strings"
	"sync"

	"github.com/godbus/dbus/v5"
)

const (
	notificationsService   = "org.freedesktop.Notifications"
	notificationsObject    = "/org/freedesktop/Notifications"
	notificationsInterface = "org.freedesktop.Notifications"
	notificationAppName    = "dalgona"
	defaultNotifyAction    = "default"
)

var errNoNotificationSettings = errors.New("this desktop has no known notification settings panel; open the notification settings of your desktop environment")

var notificationMarkup = strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;")

type Notifier struct {
	notifierState
	idsMu   sync.Mutex
	conn    *dbus.Conn
	markup  bool
	byTag   map[string]uint32
	byID    map[uint32]string
	signals chan *dbus.Signal
}

func NewNotifier(wake func()) *Notifier {
	return &Notifier{notifierState: newNotifierState(wake), byTag: map[string]uint32{}, byID: map[uint32]string{}}
}

func (n *Notifier) Request(ctx context.Context) (NotificationPermission, error) {
	return n.Refresh(ctx)
}

func (n *Notifier) Refresh(ctx context.Context) (NotificationPermission, error) {
	if _, err := n.connect(ctx); err != nil {
		n.settle(PermissionUnavailable)
		return PermissionUnavailable, err
	}
	n.settle(PermissionGranted)
	return PermissionGranted, nil
}

func (n *Notifier) Send(ctx context.Context, note Notification) error {
	if n.Permission() != PermissionGranted {
		return ErrNotificationsOff
	}
	conn, err := n.connect(ctx)
	if err != nil {
		return err
	}
	n.idsMu.Lock()
	replaces, body := n.byTag[note.Tag], note.Body
	if n.markup {
		body = notificationMarkup.Replace(body)
	}
	n.idsMu.Unlock()
	var id uint32
	err = conn.Object(notificationsService, notificationsObject).CallWithContext(ctx, notificationsInterface+".Notify", 0,
		notificationAppName, replaces, "", note.Title, body, []string{defaultNotifyAction, "Open"}, map[string]dbus.Variant{}, int32(-1)).Store(&id)
	if err != nil {
		return fmt.Errorf("show the desktop notification: %w", err)
	}
	n.idsMu.Lock()
	delete(n.byID, replaces)
	n.byTag[note.Tag], n.byID[id] = id, note.Tag
	n.idsMu.Unlock()
	return nil
}

func (n *Notifier) Close() error {
	n.idsMu.Lock()
	conn := n.conn
	n.conn = nil
	n.idsMu.Unlock()
	if conn == nil {
		return nil
	}
	return conn.Close()
}

func (n *Notifier) OpenSettings() error {
	desktop := strings.ToUpper(os.Getenv("XDG_CURRENT_DESKTOP"))
	var cmd *exec.Cmd
	switch {
	case strings.Contains(desktop, "GNOME"):
		cmd = exec.Command("gnome-control-center", "notifications")
	case strings.Contains(desktop, "KDE"):
		cmd = exec.Command("systemsettings", "kcm_notifications")
	default:
		return errNoNotificationSettings
	}
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("open the notification settings: %w", err)
	}
	go func() { _ = cmd.Wait() }()
	return nil
}

func (n *Notifier) connect(ctx context.Context) (*dbus.Conn, error) {
	n.idsMu.Lock()
	defer n.idsMu.Unlock()
	if n.conn != nil {
		return n.conn, nil
	}
	conn, err := dbus.ConnectSessionBus()
	if err != nil {
		return nil, fmt.Errorf("connect to the session bus for desktop notifications: %w", err)
	}
	var capabilities []string
	if err := conn.Object(notificationsService, notificationsObject).CallWithContext(ctx, notificationsInterface+".GetCapabilities", 0).Store(&capabilities); err != nil {
		_ = conn.Close()
		return nil, fmt.Errorf("no desktop notification service answered; start your desktop's notification server: %w", err)
	}
	if err := conn.AddMatchSignal(dbus.WithMatchObjectPath(notificationsObject), dbus.WithMatchInterface(notificationsInterface)); err != nil {
		_ = conn.Close()
		return nil, fmt.Errorf("subscribe to desktop notification clicks: %w", err)
	}
	n.conn, n.markup = conn, slices.Contains(capabilities, "body-markup")
	n.signals = make(chan *dbus.Signal, notificationActivationQueue)
	conn.Signal(n.signals)
	go n.watch(n.signals)
	return conn, nil
}

func (n *Notifier) watch(signals <-chan *dbus.Signal) {
	for s := range signals {
		var id uint32
		if len(s.Body) == 0 || dbus.Store(s.Body[:1], &id) != nil {
			continue
		}
		switch s.Name {
		case notificationsInterface + ".ActionInvoked":
			n.invoked(id, s.Body)
		case notificationsInterface + ".NotificationClosed":
			n.forget(id)
		}
	}
}

func (n *Notifier) invoked(id uint32, body []any) {
	var action string
	if len(body) < 2 || dbus.Store(body[1:2], &action) != nil || action != defaultNotifyAction {
		return
	}
	n.idsMu.Lock()
	tag, ok := n.byID[id]
	n.idsMu.Unlock()
	if ok {
		n.activate(tag)
	}
}

func (n *Notifier) forget(id uint32) {
	n.idsMu.Lock()
	defer n.idsMu.Unlock()
	if tag, ok := n.byID[id]; ok {
		delete(n.byID, id)
		if n.byTag[tag] == id {
			delete(n.byTag, tag)
		}
	}
}
