//go:build gui && guiintegration && (amd64 || arm64)

package platform

import (
	"context"
	"slices"
	"testing"
	"time"
	"unsafe"
)

const testToastNotifierHide = 7

func shownToastKeys(t *testing.T, n *Notifier) []string {
	t.Helper()
	keys, err := notifierCall(t.Context(), n, func(s *toastSession) []string {
		var keys []string
		for key := range s.shown {
			keys = append(keys, key)
		}
		return keys
	})
	if err != nil {
		t.Fatal(err)
	}
	return keys
}

func TestWindowsNotifierShowsReplacesAndRetiresToasts(t *testing.T) {
	n := NewNotifier(nil)
	defer func() {
		if err := n.Close(); err != nil {
			t.Error(err)
		}
	}()
	if permission, err := n.Refresh(t.Context()); permission != PermissionGranted {
		t.Skipf("Windows reports notification permission %d (%v); install the dalgona MSI so the %s Start menu shortcut exists, then allow its notifications", permission, err, toastAppID)
	}
	for _, body := range []string{"first notice", "replacement & <escaped> notice"} {
		if err := n.Send(t.Context(), Notification{Title: "Done", Body: body, Tag: "smoke-thread"}); err != nil {
			t.Fatal(err)
		}
	}
	if keys := shownToastKeys(t, n); !slices.Equal(keys, []string{"smoke-thread"}) {
		t.Fatalf("shown toasts %q, want the replacement alone under smoke-thread", keys)
	}
	hidden, err := notifierCall(t.Context(), n, func(s *toastSession) uintptr {
		return s.notifier.call(testToastNotifierHide, unsafe.Pointer(s.shown["smoke-thread"].toast)) //nolint:gosec // Hide takes the IToastNotification pointer
	})
	if err != nil || winrtFailed(hidden) {
		t.Fatalf("hide the toast: %v, HRESULT 0x%08X", err, hidden&winrtHResultBits)
	}
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	check := time.NewTicker(20 * time.Millisecond)
	defer check.Stop()
	for len(shownToastKeys(t, n)) != 0 {
		select {
		case <-ctx.Done():
			t.Fatal("the Dismissed handler never retired the hidden toast")
		case <-check.C:
		}
	}
}
