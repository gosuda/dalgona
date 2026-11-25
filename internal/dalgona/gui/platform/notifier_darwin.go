//go:build darwin

package platform

// #cgo CFLAGS: -fobjc-arc -x objective-c
// #cgo LDFLAGS: -framework AppKit -framework UserNotifications
// #include <stdlib.h>
// #include "notifier_darwin.h"
import "C"

import (
	"context"
	"errors"
	"fmt"
	"os"
	"sync"
	"syscall"
	"unsafe"
)

var (
	errNotificationBundle   = errors.New("desktop notifications need the packaged dalgona.app; open dalgona from its app bundle instead of go run")
	errNotifierClosed       = errors.New("the desktop notifier is closed; reopen the window to send notifications")
	errNotificationSettings = errors.New("the System Settings Notifications pane did not open; open System Settings, choose Notifications, then choose dalgona")
)

type Notifier struct {
	notifierState
	native  C.uintptr_t
	broken  error
	served  chan struct{}
	readErr error
	closeMu sync.Mutex
	closed  bool
}

type appKitAuthorization struct {
	permission NotificationPermission
	err        error
}

func NewNotifier(wake func()) *Notifier {
	n := &Notifier{notifierState: newNotifierState(wake)}
	if C.dalgona_notifier_bundled() == 0 {
		n.disable(errNotificationBundle)
		return n
	}
	var native C.uintptr_t
	var activations C.int
	if code := C.dalgona_notifier_open(&native, &activations); code != 0 {
		n.disable(fmt.Errorf("listen for notification clicks: create the click pipe: %w; close unused files, then restart dalgona", syscall.Errno(code))) //nolint:gosec // dalgona_notifier_open returns a positive errno value
		return n
	}
	n.native, n.served = native, make(chan struct{})
	clicks := os.NewFile(uintptr(activations), "appkit notification clicks") //nolint:gosec // pipe(2) returned a non-negative descriptor
	go func() {
		defer close(n.served)
		n.readErr = serveNotificationActivations(clicks, n.activate)
		_ = clicks.Close()
	}()
	return n
}

func (n *Notifier) disable(err error) {
	n.broken = err
	n.settle(PermissionUnavailable)
}

func (n *Notifier) usable() error {
	n.closeMu.Lock()
	defer n.closeMu.Unlock()
	if n.closed {
		return errNotifierClosed
	}
	return n.broken
}

func (n *Notifier) Request(ctx context.Context) (NotificationPermission, error) {
	return n.authorize(ctx, func(failure **C.char) C.uint8_t { return C.dalgona_notifier_request(failure) })
}

func (n *Notifier) Refresh(ctx context.Context) (NotificationPermission, error) {
	return n.authorize(ctx, func(failure **C.char) C.uint8_t { return C.dalgona_notifier_settings(failure) })
}

func (n *Notifier) authorize(ctx context.Context, query func(**C.char) C.uint8_t) (NotificationPermission, error) {
	if err := n.usable(); err != nil {
		return PermissionUnavailable, err
	}
	result, err := awaitNative(ctx, func() appKitAuthorization {
		var failure *C.char
		status := query(&failure)
		permission, queryErr := appKitNotificationPermission(int(status), takeNativeText(failure))
		n.settle(permission)
		return appKitAuthorization{permission, queryErr}
	})
	if err != nil {
		return n.Permission(), err
	}
	return result.permission, result.err
}

func (n *Notifier) Send(ctx context.Context, note Notification) error {
	if err := n.usable(); err != nil {
		return err
	}
	if n.Permission() != PermissionGranted {
		return ErrNotificationsOff
	}
	if err := checkAppKitNotification(note); err != nil {
		return err
	}
	failure, err := awaitNative(ctx, func() string {
		tag, tagLength := nativeBytes(note.Tag)
		title, titleLength := nativeBytes(note.Title)
		body, bodyLength := nativeBytes(note.Body)
		return takeNativeText(C.dalgona_notifier_send(tag, tagLength, title, titleLength, body, bodyLength))
	})
	if err != nil {
		return err
	}
	if failure != "" {
		return fmt.Errorf("show the desktop notification: macOS refused it: %s; check dalgona in System Settings, Notifications", failure)
	}
	return nil
}

func (n *Notifier) OpenSettings() error {
	if C.dalgona_notifier_open_settings() == 0 {
		return errNotificationSettings
	}
	return nil
}

func (n *Notifier) Close() error {
	n.closeMu.Lock()
	if n.closed {
		n.closeMu.Unlock()
		return nil
	}
	n.closed = true
	n.closeMu.Unlock()
	if n.served == nil {
		return nil
	}
	C.dalgona_notifier_close(n.native)
	<-n.served
	return n.readErr
}

func nativeBytes(text string) (*C.uint8_t, C.size_t) {
	return (*C.uint8_t)(unsafe.Pointer(unsafe.StringData(text))), C.size_t(len(text)) //nolint:gosec // the native call copies the bytes before it returns
}

func takeNativeText(text *C.char) string {
	if text == nil {
		return ""
	}
	defer C.free(unsafe.Pointer(text)) //nolint:gosec // the native side allocated the text with strdup
	return C.GoString(text)
}
