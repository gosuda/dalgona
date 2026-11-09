package agent

import (
	"testing"

	"github.com/gosuda/dalgona/internal/model/cursor/wire"
)

func dropAfterInBandExec(c *cursorConn) {
	c.send(&wire.AgentServerMessage{Exec: &wire.ExecServerMessage{
		ID: 9, ExecIDView: []byte("exec-1"), Shell: &wire.ShellArgs{CommandView: []byte("echo in-band-side-effect"), ToolCallIDView: []byte("call-1")},
	}})
	c.awaitExecReply()
}

func TestInBandEventsSurviveStreamFailure(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withCursor(t, dropAfterInBandExec))
	thread := h.start()
	h.submit(thread, "run it in band")
	h.waitIdle(thread)
	events := h.events(thread.ID())
	if !started(events, "call-1") {
		t.Fatalf("agent: in-band tool.started lost on stream failure: %v", h.types(thread.ID()))
	}
	outputs := completedOutputs(events)
	completed, ok := outputs["call-1"]
	if !ok {
		t.Fatalf("agent: in-band tool.completed lost on stream failure: %v", h.types(thread.ID()))
	}
	if !completed.OK {
		t.Fatalf("agent: in-band result not recorded as success: %+v", completed)
	}
}
