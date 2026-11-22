package tui

import (
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
)

func TestRetryWaitShowsCountdownAndEscStops(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	caller := new(fakeCaller)
	m := newTestModel(caller, Options{})
	m.conn.clock = func() time.Time { return now }
	m.screen.width = 120
	m, _ = drive(t, m, notificationMsg{method: "turn/started", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)})
	m, _ = drive(t, m, notificationMsg{method: "dalgona/retry/wait", params: []byte(`{"threadId":"t1","turnId":"turn-1","remainingMs":125000}`)})
	want := "rate limited · retrying in 2m 05s · esc stops"
	if !strings.Contains(stripANSI(m.liveRow()), want) {
		t.Fatalf("retry wait row = %q, want %q", m.liveRow(), want)
	}
	m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
	runCommand(t, m, command, "Esc did not send turn/interrupt")
	if last := caller.calls[len(caller.calls)-1]; last.method != "turn/interrupt" {
		t.Fatalf("Esc sent %q, want turn/interrupt", last.method)
	}
}

func TestRetryWaitCountdownShowsOverRetryNotice(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	caller := new(fakeCaller)
	m := newTestModel(caller, Options{})
	m.conn.clock = func() time.Time { return now }
	m.screen.width = 120
	m, _ = drive(t, m, notificationMsg{method: "turn/started", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)})
	m, _ = drive(t, m, notificationMsg{method: "error", params: []byte(`{"threadId":"t1","turnId":"turn-1","willRetry":true,"error":{"message":"model request failed (retry, status 429); retry 1 in 120000 ms"}}`)})
	if got := stripANSI(m.liveRow()); !strings.Contains(got, "model request failed (retry, status 429)") {
		t.Fatalf("live row with a retry notice and no wait = %q, want the retry notice", got)
	}
	m, _ = drive(t, m, notificationMsg{method: "dalgona/retry/wait", params: []byte(`{"threadId":"t1","turnId":"turn-1","remainingMs":120000}`)})
	want := "rate limited · retrying in 2m 00s · esc stops"
	if got := stripANSI(m.liveRow()); !strings.Contains(got, want) {
		t.Fatalf("live row during a retry wait = %q, want %q", got, want)
	}
	now = now.Add(126 * time.Second)
	if got := stripANSI(m.liveRow()); !strings.Contains(got, "rate limited · retrying in 0m 00s · esc stops") {
		t.Fatalf("live row after the estimate passed = %q, want the zero countdown", got)
	}
	m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
	runCommand(t, m, command, "Esc did not send turn/interrupt")
	if last := caller.calls[len(caller.calls)-1]; last.method != "turn/interrupt" {
		t.Fatalf("Esc sent %q, want turn/interrupt", last.method)
	}
	m, _ = drive(t, m, notificationMsg{method: "dalgona/retry/wait", params: []byte(`{"threadId":"t1","turnId":"turn-1","remainingMs":0}`)})
	got := stripANSI(m.liveRow())
	if strings.Contains(got, "rate limited") {
		t.Fatalf("live row after the zero report = %q, want no countdown", got)
	}
	if !strings.Contains(got, "Working") {
		t.Fatalf("live row after the zero report = %q, want the live working row", got)
	}
}

func TestTickRunsOnlyWhileLive(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	if !m.screen.ticking {
		t.Fatal("a running turn did not start the tick chain")
	}
	m, _ = drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"completed"}}`)})
	m, command := drive(t, m, m.screen.spin.Tick())
	if command != nil || m.screen.ticking {
		t.Fatalf("tick after the turn ended: command %v, ticking %v", command, m.screen.ticking)
	}
}

func TestTickRunsInAwaitingInput(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.thread.state = "awaiting input"
	if m.scheduleTick() == nil {
		t.Fatal("awaiting input did not schedule the tick chain")
	}
	m, _ = drive(t, m, tea.EnvMsg([]string{"DAL_NO_MOTION=1"}))
	m.screen.ticking = false
	if m.scheduleTick() == nil || m.screen.spin.Spinner.FPS != time.Second {
		t.Fatalf("awaiting input with motion off: FPS %v", m.screen.spin.Spinner.FPS)
	}
}

func TestNoMotionTicksOncePerSecond(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	m, _ = drive(t, m, tea.EnvMsg([]string{"DAL_NO_MOTION=1"}))
	if m.screen.spin.Spinner.FPS != time.Second {
		t.Fatalf("motion off ticks every %v, want 1s", m.screen.spin.Spinner.FPS)
	}
	now = now.Add(12 * time.Second)
	before := stripANSI(m.liveRow())
	now = now.Add(time.Second)
	m, _ = drive(t, m, m.screen.spin.Tick())
	after := stripANSI(m.liveRow())
	if !strings.Contains(before, "\u25aa") || !strings.Contains(after, "\u25aa") {
		t.Fatalf("the dot left its first frame: %q then %q", before, after)
	}
	if !strings.Contains(before, "12s") || !strings.Contains(after, "13s") {
		t.Fatalf("elapsed did not tick: %q then %q", before, after)
	}
}

func TestKeywordMarkStartsNoTick(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	for _, r := range "ultrathink" {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	if m.screen.ticking {
		t.Fatal("typing a keyword started the tick chain")
	}
	if m.scheduleTick() != nil {
		t.Fatal("an idle model schedules a tick")
	}
}

func TestRetryCountdownRoundsUpToTheWholeSecond(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := newTestModel(new(fakeCaller), Options{})
	m.conn.clock = func() time.Time { return now }
	m.screen.width = 120
	m, _ = drive(t, m, notificationMsg{method: "turn/started", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)})
	m, _ = drive(t, m, notificationMsg{method: "dalgona/retry/wait", params: []byte(`{"threadId":"t1","turnId":"turn-1","remainingMs":120000}`)})
	steps := []struct {
		after time.Duration
		want  string
	}{
		{time.Millisecond, "2m 00s"},
		{time.Second, "1m 59s"},
		{118 * time.Second, "0m 01s"},
		{998 * time.Millisecond, "0m 01s"},
		{time.Millisecond, "0m 00s"},
	}
	for _, step := range steps {
		now = now.Add(step.after)
		want := "rate limited · retrying in " + step.want + " · esc stops"
		if got := stripANSI(m.liveRow()); !strings.Contains(got, want) {
			t.Fatalf("live row %v before the retry = %q, want %q", m.live.retryUntil.Sub(now), got, want)
		}
	}
}
