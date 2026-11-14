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

func mustAwaitExecStdout(c *cursorConn) string {
	timer := time.NewTimer(10 * time.Second)
	defer timer.Stop()
	for {
		select {
		case message, ok := <-c.inbound:
			if !ok {
				c.t.Error("cursor client closed before the exec reply")
				return ""
			}
			if message.Exec != nil {
				return shellStdout(message.Exec)
			}
		case <-timer.C:
			c.t.Error("in-band exec reply timed out")
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
	h, thread, w := inBandHarness(t)
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

func runEmptyCursorStep(t *testing.T, w *worker) {
	t.Helper()
	ctx := t.Context()
	v, messages, ctxErr := w.context(ctx)
	if ctxErr != nil {
		t.Fatalf("worker context: %v", ctxErr)
	}
	if stepErr := w.step(ctx, v, messages); stepErr != nil {
		t.Fatalf("step with empty reply failed: %v", stepErr)
	}
}

func checkLaterCursorTurn(t *testing.T, h *harness, thread *Thread) {
	t.Helper()
	events := h.events(thread.ID())
	if !started(events, "call-1") {
		t.Fatal("later in-band call never reached tool.started")
	}
	out := completedOutputs(events)["call-1"]
	if !out.OK {
		t.Fatalf("later in-band completion = %+v, want OK", out)
	}
	if got := resolutions(events)["call-1"]; got.Decision == "cancel" {
		t.Fatalf("later in-band resolution = %+v, want accept", got)
	}
	if status := lastTurn(events).Status; status != statusCompleted {
		t.Fatalf("later turn status = %q, want completed", status)
	}
}

func TestInBandCancelConsumedOnEmptyReply(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withCursor(t,
		func(c *cursorConn) { c.endTurn("empty") },
		func(c *cursorConn) {
			c.send(&wire.AgentServerMessage{Exec: &wire.ExecServerMessage{
				ID: 9, ExecIDView: []byte("exec-1"), Shell: &wire.ShellArgs{CommandView: []byte("echo kept"), ToolCallIDView: []byte("call-1")},
			}})
			mustAwaitExecStdout(c)
			c.endTurn("kept going")
		},
	))
	thread := h.start()
	ctx := t.Context()
	if _, appendErr := thread.appendEvents(ctx, event.TurnStarted{TurnID: "turn-test"}); appendErr != nil {
		t.Fatalf("start turn: %v", appendErr)
	}
	w := &worker{t: thread, turnID: "turn-test"}
	if primeErr := w.prime(ctx); primeErr != nil {
		t.Fatalf("prime worker: %v", primeErr)
	}
	w.inBand = newInBandCalls()
	w.inBand.markCancel()
	runEmptyCursorStep(t, w)
	if w.inBand.takeCancel() {
		t.Fatal("cancel flag survived a step with an empty reply")
	}
	if _, endErr := thread.appendEvents(ctx, event.TurnCompleted{TurnID: "turn-test", Status: statusInterrupted}); endErr != nil {
		t.Fatalf("end turn: %v", endErr)
	}
	h.submit(thread, "run it in band")
	h.waitIdle(thread)
	checkLaterCursorTurn(t, h, thread)
}
