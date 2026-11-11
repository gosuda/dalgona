package agent

import (
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func appendStartedWithoutCompletionBetweenRepeats(t *testing.T, thread *Thread) {
	t.Helper()
	appendGuardHistory(t, thread, startedCall("d1", "exec", "exec", `{"command":"echo other"}`), nil)
}

func TestLoopGuardStartedWithoutCompletionDoesNotReset(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	appendStartedWithoutCompletionBetweenRepeats(t, thread)
	w := guardWorker(t, thread)
	repeated, err := w.repeated("read", jsontext.Value(`{"path":"a"}`))
	if err != nil {
		t.Fatal(err)
	}
	if !repeated {
		t.Fatal("started call without completion reset the loop guard")
	}
}

func TestLoopGuardReadBetweenExecRepeatsDoesNotReset(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalYolo))
	h.write("b", "data\n")
	h.model.push(
		calls(call{id: "e1", name: "exec", args: `{"command":"echo hi"}`}),
		calls(call{id: "e2", name: "exec", args: `{"command":"echo hi"}`}),
		calls(call{id: "e3", name: "exec", args: `{"command":"echo hi"}`}),
		calls(call{id: "e4", name: "exec", args: `{"command":"echo hi"}`}),
		calls(call{id: "r1", name: "read", args: `{"path":"b"}`}),
		calls(call{id: "e5", name: "exec", args: `{"command":"echo hi"}`}),
		text("done"),
	)
	thread := h.start()
	h.submit(thread, "retest")
	h.waitIdle(thread)
	outputs := completedOutputs(h.events(thread.ID()))
	for _, id := range []string{"e1", "e2", "e3", "e4"} {
		if !outputs[id].OK {
			t.Fatalf("%s declined: %+v", id, outputs[id])
		}
	}
	if !outputs["r1"].OK {
		t.Fatalf("read declined: %+v", outputs["r1"])
	}
	if fifth := outputs["e5"]; fifth.OK || fifth.Output != loopGuardMessage {
		t.Fatalf("fifth exec after read ran: %+v", fifth)
	}
}
