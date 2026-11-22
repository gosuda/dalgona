package tui

import (
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
)

type elapsedClock struct {
	now time.Time
}

func (c *elapsedClock) advance(d time.Duration) {
	c.now = c.now.Add(d)
}

func elapsedField(m model) string {
	return strings.TrimSpace(stripANSI(m.liveElapsed()))
}

func requireElapsed(t *testing.T, m model, want, step string) {
	t.Helper()
	if got := elapsedField(m); got != want {
		t.Fatalf("%s: elapsed field = %q, want %q", step, got, want)
	}
}

func turnStartedAt(turn string, at time.Time) notificationMsg {
	return notificationMsg{method: "turn/started", params: fmt.Appendf(nil, `{"threadId":"t1","turn":{"id":%q,"items":[],"status":"inProgress","startedAt":%d}}`, turn, at.Unix())}
}

func turnCompletedMsg(turn string) notificationMsg {
	return notificationMsg{method: "turn/completed", params: fmt.Appendf(nil, `{"threadId":"t1","turn":{"id":%q,"items":[],"status":"completed"}}`, turn)}
}

func elapsedModel(t *testing.T) (model, *elapsedClock) {
	t.Helper()
	clock := &elapsedClock{now: time.Date(2026, 11, 12, 10, 0, 0, 700*int(time.Millisecond), time.UTC)}
	m := newTestModel(new(numberedTurns), Options{})
	m.conn.clock = func() time.Time { return clock.now }
	m.thread.id = "t1"
	m.screen.width = 80
	m.input.composer.SetWidth(80)
	return m, clock
}

func sendTyped(t *testing.T, m model, text string) model {
	t.Helper()
	m = typeText(t, m, text)
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	if command == nil {
		t.Fatalf("Enter on %q sent nothing", text)
	}
	m, _ = drive(t, m, command())
	return m
}

func TestElapsedOfSentTurnStartsAtZeroDespiteWholeSecondStart(t *testing.T) {
	t.Parallel()
	m, clock := elapsedModel(t)
	m = sendTyped(t, m, "a")
	requireElapsed(t, m, "0 ms", "after Enter")
	clock.advance(30 * time.Millisecond)
	m, _ = drive(t, m, turnStartedAt("turn-1", clock.now))
	requireElapsed(t, m, "30 ms", "after a turn/started with a whole-second startedAt")
	clock.advance(970 * time.Millisecond)
	requireElapsed(t, m, "1s", "one second after Enter")
}

func TestElapsedOfNextTurnDropsPreviousTurnStart(t *testing.T) {
	t.Parallel()
	m, clock := elapsedModel(t)
	m = sendTyped(t, m, "a")
	m, _ = drive(t, m, turnStartedAt("turn-1", clock.now))
	clock.advance(5 * time.Second)
	requireElapsed(t, m, "5s", "first turn")
	m, _ = drive(t, m, turnCompletedMsg("turn-1"))
	clock.advance(time.Second)
	m = sendTyped(t, m, "b")
	requireElapsed(t, m, "0 ms", "first frame of the second turn")
	clock.advance(79 * time.Millisecond)
	m, _ = drive(t, m, turnStartedAt("turn-2", clock.now))
	requireElapsed(t, m, "79 ms", "second turn after its turn/started")
}

func TestElapsedOfQueuedFollowupStartsAtZero(t *testing.T) {
	t.Parallel()
	m, clock := elapsedModel(t)
	m = sendTyped(t, m, "a")
	m, _ = drive(t, m, turnStartedAt("turn-1", clock.now))
	m = typeText(t, m, "b")
	m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	if len(m.queue) != 1 {
		t.Fatalf("queue holds %d follow-ups, want 1", len(m.queue))
	}
	clock.advance(5 * time.Second)
	m, command := drive(t, m, turnCompletedMsg("turn-1"))
	if !m.turnLive() {
		t.Fatal("the queued follow-up did not start a turn")
	}
	requireElapsed(t, m, "0 ms", "first frame of the queued follow-up")
	if command != nil {
		m, _ = drive(t, m, command())
	}
	clock.advance(40 * time.Millisecond)
	m, _ = drive(t, m, turnStartedAt("turn-2", clock.now))
	requireElapsed(t, m, "40 ms", "queued follow-up after its turn/started")
}

func TestElapsedAfterFailedSendStartsAtTurnStarted(t *testing.T) {
	t.Parallel()
	m, clock := elapsedModel(t)
	m = typeText(t, m, "a")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, resultMsg{op: "send", text: "a", err: errors.New("broken pipe")})
	clock.advance(3 * time.Second)
	m, _ = drive(t, m, turnStartedAt("turn-9", clock.now))
	requireElapsed(t, m, "0 ms", "turn/started after a failed send")
}

func TestElapsedOfUnsentTurnDropsPreviousTurnStart(t *testing.T) {
	t.Parallel()
	m, clock := elapsedModel(t)
	m = sendTyped(t, m, "a")
	m, _ = drive(t, m, turnStartedAt("turn-1", clock.now))
	clock.advance(5 * time.Second)
	m, _ = drive(t, m, turnCompletedMsg("turn-1"))
	clock.advance(4 * time.Second)
	m, _ = drive(t, m, turnStartedAt("turn-2", clock.now))
	requireElapsed(t, m, "0 ms", "turn/started of a turn this client did not send")
}
