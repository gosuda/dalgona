package tui

import (
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func TestLateTurnAcceptedDoesNotReenterRunning(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.thread.turnID = "turn-1"
	m.thread.state = stateRunning
	m, _ = drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"completed"}}`)})
	if m.thread.state != stateIdle {
		t.Fatalf("completed turn left state = %q, want idle", m.thread.state)
	}
	m, _ = drive(t, m, resultMsg{op: "send", value: &rpc.TurnStartResponse{Turn: rpc.Turn{ID: "turn-1", Status: "inProgress"}}})
	if m.thread.state != stateIdle {
		t.Fatalf("late turn/start result set state = %q, want idle", m.thread.state)
	}
	if m.thread.turnID != "" {
		t.Fatalf("late turn/start result set turnID = %q, want empty", m.thread.turnID)
	}
	m.input.composer.SetValue("next")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	if command == nil {
		t.Fatal("Enter after completed turn sent nothing")
	}
	m = runCommand(t, m, command, "typed text after completed turn did not start a new turn")
	last := caller.calls[len(caller.calls)-1]
	params, ok := last.params.(rpc.TurnStartParams)
	if last.method != "turn/start" || !ok || params.Input[0].Text != "next" {
		t.Fatalf("typed text after completed turn = %#v, want turn/start", last)
	}
}

func TestCompletedTurnsClearOnThreadSwitch(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.thread.id = "t1"
	m.thread.turnID = "turn-1"
	m.thread.state = stateRunning
	m, _ = drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"completed"}}`)})
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Thread: rpc.Thread{ID: "t2"}}})
	if len(m.thread.completedTurns) != 0 {
		t.Fatalf("completed turns survived /new: %v", m.thread.completedTurns)
	}
}
