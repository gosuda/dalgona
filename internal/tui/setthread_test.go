package tui

import (
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func TestSetThreadClearsFollowups(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.threadID = "t1"
	m.followups = []string{"stale"}
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Thread: rpc.Thread{ID: "t2"}}})
	if len(m.followups) != 0 {
		t.Fatalf("followups survived /new: %q", m.followups)
	}
	m.composer.SetValue("queued")
	caller := new(fakeCaller)
	m.client = caller
	m, command := drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	m = runCommand(t, m, command, "queued text after /new did not start a turn")
	params, ok := caller.calls[len(caller.calls)-1].params.(rpc.TurnStartParams)
	if len(caller.calls) != 1 || !ok || params.ThreadID != "t2" || params.Input[0].Text != "queued" {
		t.Fatalf("queued text after /new = %#v, want one turn/start on t2", caller.calls)
	}
}
