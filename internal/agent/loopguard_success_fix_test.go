package agent

import (
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

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
	appendGuardHistory(t, thread, startedCall("e1", "exec", "exec", `{"command":"echo hi"}`), some(true))
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
	appendGuardHistory(t, thread, startedCall("b1", "read", "read", `{"path":"b"}`), some(true))
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
	appendGuardHistory(t, thread, startedCall("p1", "patch", "edit", `{"path":"a"}`), some(false))
	w := guardWorker(t, thread)
	repeated, err := w.repeated("read", jsontext.Value(`{"path":"a"}`))
	if err != nil {
		t.Fatal(err)
	}
	if !repeated {
		t.Fatal("failed patch reset the loop guard")
	}
}

func some(v bool) *bool { return &v }
