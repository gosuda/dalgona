package agent

import (
	"encoding/json/jsontext"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
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

func TestInBandSecondCallDeclinedAfterCancel(t *testing.T) {
	t.Parallel()
	h, thread, w := inBandHarness(t, nil)
	ctx := t.Context()
	first, err := w.inBandEvents(ctx, model.ToolCall{ID: "c1", Name: "durable", Args: jsontext.Value(`{}`)})
	if err != nil {
		t.Fatalf("first in-band call failed: %v", err)
	}
	if !first.OK {
		t.Fatalf("first in-band call not accepted: %+v", first)
	}
	w.inBand.markCancel()
	second, err := w.inBandEvents(ctx, model.ToolCall{ID: "c2", Name: "durable", Args: jsontext.Value(`{}`)})
	if err != nil {
		t.Fatalf("second in-band call failed: %v", err)
	}
	if second.OK {
		t.Fatalf("second in-band call ran after a cancel: %+v", second)
	}
	if !strings.Contains(second.Output, "cancelled by user") {
		t.Fatalf("second in-band output = %q, want cancellation", second.Output)
	}
	events := h.events(thread.ID())
	if started(events, "c2") {
		t.Fatal("second in-band call reached tool.started after a cancel")
	}
	if got := resolutions(events)["c2"]; got.Decision != "cancel" {
		t.Fatalf("second in-band resolution = %+v, want cancel", got)
	}
}

func TestInBandCancelConsumedOnEmptyReply(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	ctx := t.Context()
	if _, err := thread.appendEvents(ctx, event.TurnStarted{TurnID: "turn-test"}); err != nil {
		t.Fatalf("start turn: %v", err)
	}
	w := &worker{t: thread, turnID: "turn-test"}
	if err := w.prime(ctx); err != nil {
		t.Fatalf("prime worker: %v", err)
	}
	w.inBand = newInBandCalls()
	w.inBand.markCancel()
	v, messages, err := w.context(ctx)
	if err != nil {
		t.Fatalf("worker context: %v", err)
	}
	if err := w.step(ctx, v, messages); err != nil {
		t.Fatalf("step with empty reply failed: %v", err)
	}
	if w.inBand.takeCancel() {
		t.Fatal("cancel flag survived a step with an empty reply")
	}
}
