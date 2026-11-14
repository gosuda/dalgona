package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func interruptedRunningModel(t *testing.T) model {
	t.Helper()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.thread.turnID = "turn-1"
	m.thread.state = stateRunning
	m = firstEscArms(t, m, caller)
	m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
	m = runCommand(t, m, command, "Esc did not interrupt the active turn")
	return m
}

func TestIdleStatusAfterInterruptKeepsStopped(t *testing.T) {
	t.Parallel()
	m := interruptedRunningModel(t)
	m, _ = drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"interrupted"}}`)})
	m, _ = drive(t, m, notificationMsg{method: "thread/status/changed", params: []byte(`{"threadId":"t1","status":{"type":"idle"},"seq":3}`)})
	if got := stripANSI(m.liveRow()); !strings.Contains(got, "Stopped") {
		t.Fatalf("live row after an interrupted turn and an idle status = %q, want Stopped", got)
	}
}

func TestInterruptNoticeEndsWithTurn(t *testing.T) {
	t.Parallel()
	m := interruptedRunningModel(t)
	if m.notice == "" {
		t.Fatal("interrupt set no notice, so the test cannot see it end")
	}
	m, _ = drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"interrupted"}}`)})
	if m.notice != "" {
		t.Fatalf("notice after the interrupted turn ended = %q, want none", m.notice)
	}
}

func TestIdleStatusAfterFailedTurnKeepsFailed(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.thread.turnID = "turn-1"
	m.thread.state = stateRunning
	m, _ = drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"failed"}}`)})
	m, _ = drive(t, m, notificationMsg{method: "thread/status/changed", params: []byte(`{"threadId":"t1","status":{"type":"idle"},"seq":3}`)})
	if got := stripANSI(m.liveRow()); !strings.Contains(got, "Failed") {
		t.Fatalf("live row after a failed turn and an idle status = %q, want Failed", got)
	}
}

func TestQuestionLiveRowSaysAnswerNotApproval(t *testing.T) {
	t.Parallel()
	m, _ := openQuestion(t, questionParamsJSON)
	got := stripANSI(m.liveRow())
	if !strings.Contains(got, "Waiting for your answer") || strings.Contains(got, "approval") {
		t.Fatalf("live row while a question waits = %q, want the answer wording", got)
	}
}
