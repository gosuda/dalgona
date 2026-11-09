package agent

import (
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model/cursor/wire"
)

func cancelExecScript(stdout chan<- string) func(*cursorConn) {
	return func(c *cursorConn) {
		c.send(&wire.AgentServerMessage{Exec: &wire.ExecServerMessage{
			ID: 9, ExecIDView: []byte("exec-1"), Shell: &wire.ShellArgs{CommandView: []byte("echo cancelled"), ToolCallIDView: []byte("call-1")},
		}})
		stdout <- awaitExecStdout(c)
		c.endTurn("done")
	}
}

func awaitExecStdout(c *cursorConn) string {
	timer := time.NewTimer(10 * time.Second)
	defer timer.Stop()
	for {
		select {
		case message, ok := <-c.inbound:
			if !ok {
				return ""
			}
			if message.Exec != nil {
				return shellStdout(message.Exec)
			}
		case <-timer.C:
			return ""
		}
	}
}

func TestInBandCancelInterruptsTurn(t *testing.T) {
	t.Parallel()
	stdout := make(chan string, 1)
	h := newHarness(t, withCursor(t,
		cancelExecScript(stdout),
		func(c *cursorConn) { c.endTurn("kept going") },
	), withApproval(config.ApprovalAsk))
	thread := h.start()
	client := answerApprovals(t, thread, func(ApprovalPrompt) Reply { return ReplyCancel })
	h.submit(thread, "run it in band")
	h.waitIdle(thread)
	client.stop()
	checkInBandCancel(t, h, thread, stdout)
}

func checkInBandCancel(t *testing.T, h *harness, thread *Thread, stdout <-chan string) {
	t.Helper()
	if got := <-stdout; got != "" {
		t.Fatalf("cancelled in-band call executed a shell: %q", got)
	}
	events := h.events(thread.ID())
	if resolved := resolutions(events)["call-1"]; resolved.Decision != "cancel" {
		t.Fatalf("resolution = %+v", resolved)
	}
	if started(events, "call-1") {
		t.Fatal("cancelled call reached tool.started")
	}
	if status := lastTurn(events).Status; status != statusInterrupted {
		t.Fatalf("turn status after in-band cancel = %q, want interrupted", status)
	}
}
