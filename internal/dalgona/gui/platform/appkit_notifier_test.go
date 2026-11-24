package platform

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"slices"
	"strings"
	"testing"
)

func clickRecord(tag string) []byte {
	return append(binary.LittleEndian.AppendUint32(nil, appKitIndex(len(tag))), tag...)
}

func TestNotificationActivationsArriveInOrder(t *testing.T) {
	var tags []string
	stream := slices.Concat(clickRecord("thread-1"), clickRecord("스레드 2"))
	if err := serveNotificationActivations(bytes.NewReader(stream), func(tag string) { tags = append(tags, tag) }); err != nil {
		t.Fatal(err)
	}
	if want := []string{"thread-1", "스레드 2"}; !slices.Equal(tags, want) {
		t.Fatalf("activations %q, want %q", tags, want)
	}
}

func TestNotificationActivationsRefuseMalformedRecords(t *testing.T) {
	cases := map[string][]byte{
		"empty tag":      clickRecord(""),
		"oversize tag":   clickRecord(strings.Repeat("t", appKitNotificationTagLimit+1)),
		"truncated tag":  clickRecord("thread-1")[:7],
		"invalid UTF-8":  clickRecord("a\xffb"),
		"partial header": {3, 0},
	}
	for name, stream := range cases {
		var tags []string
		err := serveNotificationActivations(bytes.NewReader(stream), func(tag string) { tags = append(tags, tag) })
		if err == nil || len(tags) != 0 {
			t.Errorf("%s: error %v, activations %q", name, err, tags)
		}
	}
}

func TestNotificationPermissionFollowsMacOSStatus(t *testing.T) {
	cases := []struct {
		status int
		want   NotificationPermission
		fails  bool
	}{
		{appKitAuthorizationNotDetermined, PermissionUndecided, false},
		{appKitAuthorizationDenied, PermissionDenied, false},
		{appKitAuthorizationAuthorized, PermissionGranted, false},
		{appKitAuthorizationProvisional, PermissionGranted, false},
		{appKitAuthorizationUnknown, PermissionUnavailable, true},
		{-1, PermissionUnavailable, true},
	}
	for _, tc := range cases {
		got, err := appKitNotificationPermission(tc.status, "")
		if got != tc.want || (err != nil) != tc.fails {
			t.Errorf("status %d: %d, %v; want %d", tc.status, got, err, tc.want)
		}
	}
	got, err := appKitNotificationPermission(appKitAuthorizationDenied, "Notifications are not allowed for this application")
	if got != PermissionDenied || err == nil || !strings.Contains(err.Error(), "not allowed") {
		t.Fatalf("a failed request: %d, %v; want denied with the macOS reason", got, err)
	}
}

func TestCheckAppKitNotificationKeepsClicksRoutable(t *testing.T) {
	if err := checkAppKitNotification(Notification{Title: "Done", Body: "한글 thread", Tag: "thread-1"}); err != nil {
		t.Fatal(err)
	}
	cases := map[string]Notification{
		"no tag":             {Title: "Done", Body: "x"},
		"tag over the limit": {Title: "Done", Body: "x", Tag: strings.Repeat("t", appKitNotificationTagLimit+1)},
		"invalid body":       {Title: "Done", Body: "\xff", Tag: "thread-1"},
	}
	for name, note := range cases {
		if err := checkAppKitNotification(note); err == nil {
			t.Errorf("%s: the notification was accepted", name)
		}
	}
	longest := strings.Repeat("t", appKitNotificationTagLimit)
	if err := checkAppKitNotification(Notification{Title: "Done", Tag: longest}); err != nil {
		t.Fatalf("a tag at the limit: %v", err)
	}
	var tags []string
	if err := serveNotificationActivations(bytes.NewReader(clickRecord(longest)), func(tag string) { tags = append(tags, tag) }); err != nil || len(tags) != 1 {
		t.Fatalf("a click on a tag at the limit: %v, %d activations", err, len(tags))
	}
}

func TestAwaitNativeReturnsWhenTheContextEnds(t *testing.T) {
	release := make(chan struct{})
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := awaitNative(ctx, func() int { <-release; return 1 }); !errors.Is(err, context.Canceled) {
		t.Fatalf("a cancelled call: %v", err)
	}
	blocked, stop := context.WithCancel(context.Background())
	started := make(chan struct{})
	go func() {
		<-started
		stop()
	}()
	if _, err := awaitNative(blocked, func() int { close(started); <-release; return 1 }); !errors.Is(err, context.Canceled) {
		t.Fatalf("a call cancelled while running: %v", err)
	}
	close(release)
	if got, err := awaitNative(context.Background(), func() int { return 7 }); err != nil || got != 7 {
		t.Fatalf("a finished call: %d, %v", got, err)
	}
}
