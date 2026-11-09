package agent

import (
	"context"
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
)

type flakyAgent struct{}

func (flakyAgent) Spec() tool.Spec {
	return tool.Spec{Name: "fanout", Description: "subagent", Schema: jsontext.Value(`{"type":"object"}`)}
}

func (flakyAgent) Class() tool.Class { return tool.ClassAgent }

func (flakyAgent) Run(_ context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	if call.ID == "bad" {
		return tool.Result{Output: []byte("unusable"), OK: true, Meta: jsontext.Value("{invalid}")}, nil
	}
	return tool.Result{Output: []byte("child " + call.ID), OK: true}, nil
}

func TestAgentBatchKeepsSuccessfulSiblings(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.Tools = func(string, string) ([]tool.Tool, error) { return []tool.Tool{flakyAgent{}}, nil }
	})
	events := runCalls(t, h,
		call{id: "good", name: "fanout", args: `{}`},
		call{id: "bad", name: "fanout", args: `{}`},
	)
	outputs := completedOutputs(events)
	completed, ok := outputs["good"]
	if !ok || string(completed.Output) != "child good" {
		t.Fatalf("agent: successful sibling result missing: %v", outputs)
	}
}
