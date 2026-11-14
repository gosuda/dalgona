package agent

import (
	"encoding/json/jsontext"
	"fmt"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
)

func TestLoopGuardDeclinesInterleavedRepeats(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.write("a", "x\n")
	h.write("b", "y\n")
	for i := range 4 {
		h.model.push(calls(
			call{id: fmt.Sprintf("a%d", i+1), name: "read", args: `{"path":"a"}`},
			call{id: fmt.Sprintf("b%d", i+1), name: "read", args: `{"path":"b"}`},
		))
	}
	h.model.push(calls(call{id: "a5", name: "read", args: `{"path":"a"}`}))
	h.model.push(text("stopping"))
	thread := h.start()
	h.submit(thread, "loop")
	h.waitIdle(thread)
	checkInterleavedGuard(t, h, thread)
}

func checkInterleavedGuard(t *testing.T, h *harness, thread *Thread) {
	t.Helper()
	outputs := completedOutputs(h.events(thread.ID()))
	for _, id := range []string{"a1", "b1", "a2", "b2", "a3", "b3", "a4", "b4"} {
		if !outputs[id].OK {
			t.Fatalf("%s = %+v", id, outputs[id])
		}
	}
	if fifth := outputs["a5"]; fifth.OK || fifth.Output != loopGuardMessage {
		t.Fatalf("fifth interleaved a-call ran; four earlier a-starts exist: %+v", fifth)
	}
}

func TestLoopGuardAllowsRetestAfterEdit(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalYolo))
	h.write("a.txt", "v1\n")
	h.model.push(
		calls(call{id: "e1", name: "exec", args: `{"command":"echo hi"}`}),
		calls(call{id: "p1", name: "patch", args: `{"changes":[{"path":"a.txt","old":"v1","new":"v2"}]}`}),
		calls(call{id: "e2", name: "exec", args: `{"command":"echo hi"}`}),
		calls(call{id: "e3", name: "exec", args: `{"command":"echo hi"}`}),
		calls(call{id: "e4", name: "exec", args: `{"command":"echo hi"}`}),
		calls(call{id: "e5", name: "exec", args: `{"command":"echo hi"}`}),
		calls(call{id: "e6", name: "exec", args: `{"command":"echo hi"}`}),
		text("done"),
	)
	thread := h.start()
	h.submit(thread, "retest")
	h.waitIdle(thread)
	outputs := completedOutputs(h.events(thread.ID()))
	for _, id := range []string{"e1", "e2", "e3", "e4", "e5"} {
		if !outputs[id].OK {
			t.Fatalf("%s declined: %+v", id, outputs[id])
		}
	}
	if sixth := outputs["e6"]; sixth.OK || sixth.Output != loopGuardMessage {
		t.Fatalf("sixth exec after edit ran: %+v", sixth)
	}
}

func appendGuardHistory(t *testing.T, thread *Thread, different event.ToolStarted, differentOK *bool) {
	t.Helper()
	ctx := t.Context()
	if _, err := thread.appendEvents(ctx, event.TurnStarted{TurnID: "t"}); err != nil {
		t.Fatal(err)
	}
	if _, err := thread.appendEvents(ctx, event.UserMessage{ItemID: "u1", TurnID: "t", Text: "go", Kind: "prompt"}); err != nil {
		t.Fatal(err)
	}
	for _, id := range []string{"r1", "r2", "r3", "r4"} {
		started := startedCall(id, "read", "read", `{"path":"a"}`)
		if _, err := thread.appendEvents(ctx, started); err != nil {
			t.Fatal(err)
		}
		if _, err := thread.appendEvents(ctx, event.ToolCompleted{CallID: id, OK: true, Output: "ok", Recovered: "none"}); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := thread.appendEvents(ctx, different); err != nil {
		t.Fatal(err)
	}
	if differentOK != nil {
		if _, err := thread.appendEvents(ctx, event.ToolCompleted{CallID: different.CallID, OK: *differentOK, Output: "out", Recovered: "none"}); err != nil {
			t.Fatal(err)
		}
	}
}

func guardWorker(t *testing.T, thread *Thread) *worker {
	t.Helper()
	w := &worker{t: thread, turnID: "t"}
	if err := w.prime(t.Context()); err != nil {
		t.Fatal(err)
	}
	return w
}

func TestLoopGuardSuccessExecResets(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	appendGuardHistory(t, thread, startedCall("e1", "exec", "exec", `{"command":"echo hi"}`), new(true))
	w := guardWorker(t, thread)
	repeated, err := w.repeated("read", jsontext.Value(`{"path":"a"}`))
	if err != nil {
		t.Fatal(err)
	}
	if repeated {
		t.Fatal("successful exec did not reset the loop guard")
	}
}

func TestLoopGuardReadDoesNotReset(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	appendGuardHistory(t, thread, startedCall("b1", "read", "read", `{"path":"b"}`), new(true))
	w := guardWorker(t, thread)
	repeated, err := w.repeated("read", jsontext.Value(`{"path":"a"}`))
	if err != nil {
		t.Fatal(err)
	}
	if !repeated {
		t.Fatal("different-key read reset the loop guard")
	}
}

func TestLoopGuardFailedPatchDoesNotReset(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	appendGuardHistory(t, thread, startedCall("p1", "patch", "edit", `{"path":"a"}`), new(false))
	w := guardWorker(t, thread)
	repeated, err := w.repeated("read", jsontext.Value(`{"path":"a"}`))
	if err != nil {
		t.Fatal(err)
	}
	if !repeated {
		t.Fatal("failed patch reset the loop guard")
	}
}
