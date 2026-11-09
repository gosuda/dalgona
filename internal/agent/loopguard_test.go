package agent

import (
	"fmt"
	"testing"
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
