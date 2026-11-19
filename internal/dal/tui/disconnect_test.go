package tui

import (
	"errors"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
)

const connectionLostRow = "connection lost: EOF; restart the TUI, then /resume t1"

type openWork struct {
	caller   *fakeCaller
	approved chan requestAnswer
	asked    chan requestAnswer
}

func liveRowText(m model) string {
	return strings.Join(strings.Fields(stripANSI(m.liveRow())), " ")
}

func assertErrorAnswer(t *testing.T, name string, answer chan requestAnswer) {
	t.Helper()
	select {
	case got := <-answer:
		if got.err == nil {
			t.Fatalf("%s received %#v, want an error answer", name, got)
		}
	default:
		t.Fatalf("%s received no answer, so its RPC handler still blocks", name)
	}
}

func assertNoInterrupt(t *testing.T, caller *fakeCaller) {
	t.Helper()
	for _, call := range caller.calls {
		if call.method == "turn/interrupt" {
			t.Fatal("Esc after the connection dropped sent turn/interrupt")
		}
	}
}

func assertNoQuestionRow(t *testing.T, m model) {
	t.Helper()
	for key := range m.out.printed {
		if strings.HasPrefix(key, "question:") {
			t.Fatalf("the lost connection wrote the question row %q", key)
		}
	}
	for _, row := range m.out.entries {
		if strings.HasPrefix(row.key, "question:") {
			t.Fatalf("the lost connection kept the question row %q", row.key)
		}
	}
}

func waitingModel(t *testing.T) (model, openWork) {
	t.Helper()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	work := openWork{caller: new(fakeCaller), approved: make(chan requestAnswer, 1), asked: make(chan requestAnswer, 1)}
	m := newTestModel(work.caller, Options{})
	m.conn.clock = func() time.Time { return now }
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 120, Height: 30})
	m, _ = drive(t, m, notificationMsg{method: "turn/started", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)})
	m, _ = drive(t, m, notificationMsg{method: "dalgona/retry/wait", params: []byte(`{"threadId":"t1","turnId":"turn-1","remainingMs":125000}`)})
	m, _ = drive(t, m, commandApprovalRequest(work.approved))
	m, _ = drive(t, m, questionRequest(work.asked, questionParamsJSON))
	m.queue = queued("and then this")
	m.input.composer.SetValue("half a sentence")
	if _, waiting := m.retryRemaining(); !waiting || len(m.approvals) != 2 {
		t.Fatalf("setup: waiting %v, open cards %d, want a retry wait and two cards", waiting, len(m.approvals))
	}
	return m, work
}

func TestDisconnectClearsRetryWaitAndCards(t *testing.T) {
	t.Parallel()
	m, work := waitingModel(t)

	m, _ = drive(t, m, disconnectedMsg{err: errors.New("EOF")})

	if m.thread.state != stateDisconnected {
		t.Fatalf("state = %q, want %q", m.thread.state, stateDisconnected)
	}
	if _, waiting := m.retryRemaining(); waiting {
		t.Fatal("the retry wait stays open after the connection dropped")
	}
	if len(m.approvals) != 0 {
		t.Fatalf("%d cards stay open after the connection dropped", len(m.approvals))
	}
	assertErrorAnswer(t, "the approval request", work.approved)
	assertErrorAnswer(t, "the question request", work.asked)
	assertNoQuestionRow(t, m)
	row := liveRowText(m)
	if !strings.Contains(row, connectionLostRow) || strings.Contains(row, "rate limited") {
		t.Fatalf("live row = %q, want the connection notice and no countdown", row)
	}
	m, command := drive(t, m, m.screen.spin.Tick())
	if command != nil || m.screen.ticking {
		t.Fatalf("tick after the connection dropped: command %v, ticking %v, want the chain stopped", command, m.screen.ticking)
	}
}

func TestDisconnectKeepsQueueAndDraft(t *testing.T) {
	t.Parallel()
	m, _ := waitingModel(t)

	m, _ = drive(t, m, disconnectedMsg{err: errors.New("EOF")})

	if len(m.queue) != 1 || m.input.composer.Value() != "half a sentence" {
		t.Fatalf("follow-up queue %v and draft %q, want both kept", m.queue, m.input.composer.Value())
	}
}

func TestEscAfterDisconnectShowsNoticeAndSendsNoInterrupt(t *testing.T) {
	t.Parallel()
	m, work := waitingModel(t)
	m, _ = drive(t, m, disconnectedMsg{err: errors.New("EOF")})

	m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
	if command != nil {
		_, _ = drive(t, m, command())
	}

	assertNoInterrupt(t, work.caller)
	if row := liveRowText(m); !strings.Contains(row, connectionLostRow) {
		t.Fatalf("live row after Esc = %q, want the connection notice", row)
	}
}

func TestDisconnectReplacesPendingKeyHint(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := runningModel(caller)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 120, Height: 30})
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if m.hint.pending != "interrupt" {
		t.Fatalf("setup: hint %q, want the interrupt hint", m.hint.pending)
	}

	m, _ = drive(t, m, disconnectedMsg{err: errors.New("EOF")})

	row := liveRowText(m)
	if strings.Contains(row, "Esc again to interrupt") || !strings.Contains(row, connectionLostRow) {
		t.Fatalf("live row = %q, want the connection notice in place of the key hint", row)
	}
	_, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	assertNoInterrupt(t, caller)
}
