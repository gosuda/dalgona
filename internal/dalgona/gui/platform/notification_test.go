package platform

import "testing"

func TestNoticeNamesTheThreadWhileTheWindowIsUnfocused(t *testing.T) {
	away := NotificationState{Permission: PermissionGranted, Enabled: true}
	want := map[TurnEvent]string{
		TurnWaiting:     "Waiting for your reply",
		TurnCompleted:   "Done",
		TurnFailed:      "Failed",
		TurnInterrupted: "Stopped",
	}
	for event, title := range want {
		n, ok := away.Notice(event, "thr_42", "Fix the flaky test")
		if !ok || n != (Notification{Title: title, Body: "Fix the flaky test", Tag: "thr_42"}) {
			t.Errorf("event %d: %+v %v, want title %q tagged with the thread", event, n, ok, title)
		}
	}
	if n, ok := away.Notice(0, "thr_42", "x"); ok {
		t.Errorf("an unknown event produced %+v", n)
	}
}

func TestNoticeRequiresPermissionConsentAndAbsence(t *testing.T) {
	blocked := map[string]NotificationState{
		"window focused":      {Permission: PermissionGranted, Enabled: true, Focused: true},
		"user did not enable": {Permission: PermissionGranted},
		"permission denied":   {Permission: PermissionDenied, Enabled: true},
		"permission pending":  {Permission: PermissionUndecided, Enabled: true},
		"service unavailable": {Permission: PermissionUnavailable, Enabled: true},
	}
	for name, s := range blocked {
		if n, ok := s.Notice(TurnCompleted, "thr_1", "t"); ok {
			t.Errorf("%s: sent %+v", name, n)
		}
	}
	granted := NotificationState{Permission: PermissionGranted, Enabled: true}
	if n, ok := granted.Notice(TurnCompleted, "", "untitled"); ok {
		t.Errorf("a notice without a thread tag was sent: %+v", n)
	}
	if n, ok := granted.Notice(TurnCompleted, "thr_1", ""); ok {
		t.Errorf("a notice that does not name its thread was sent: %+v", n)
	}
}
