package agent

import (
	"context"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
	exectool "github.com/gosuda/dalgona/internal/tool/exec"
)

func TestAdvisorAskExecFallsBackToDecider(t *testing.T) {
	t.Parallel()
	counter := &countingDecider{}
	h := newHarness(t, withApproval(config.ApprovalAsk), func(h *harness) { h.cfg.Decider = counter })
	parent := h.start()
	child, err := h.host.Start(h.t.Context(), ThreadSpec{
		Parent: parent.ID(), Advisor: "advisor", Tools: []tool.Tool{exectool.New()}, System: "advisor",
	})
	if err != nil {
		t.Fatal(err)
	}
	updates := child.Subscribe()
	defer child.Unsubscribe(updates)
	w := &worker{t: child, turnID: "t", settings: Settings{Approval: config.ApprovalAsk, Sandbox: config.SandboxOff}}
	ctx, cancel := context.WithTimeout(h.t.Context(), 2*time.Second)
	defer cancel()
	v := w.decide(ctx, gateCall{
		call: event.ToolCall{ID: "c", Name: "exec"}, class: tool.ClassExec, command: "go version",
	}, "req-1")
	if !v.allow {
		t.Fatalf("verdict = %+v", v)
	}
	if counter.calls.Load() != 1 {
		t.Fatalf("decider calls = %d, want 1", counter.calls.Load())
	}
}
