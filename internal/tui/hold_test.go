package tui

import (
	"testing"

	tea "charm.land/bubbletea/v2"
)

func TestQueueFollowupHoldsQueueAfterInterruptedTurn(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.thread.state = stateInterrupted
	m.queue = []string{"held"}
	m.input.composer.SetValue("new text")
	m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	if len(caller.calls) != 0 {
		t.Fatalf("alt+enter released held queue after interrupt: %#v", caller.calls)
	}
	if len(m.queue) != 2 || m.queue[0] != "held" {
		t.Fatalf("followups = %q, want held first", m.queue)
	}
	if m.thread.state != stateInterrupted {
		t.Fatalf("alt+enter changed state to %q", m.thread.state)
	}
}
