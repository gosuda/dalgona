package rpc

import (
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestCapturedRequestEffortFollowsTurnInputs(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	requests := recordRequests(t, r)
	c := r.connect()
	var started ThreadStartResponse
	c.ok("thread/start", map[string]any{
		"cwd": r.cwd, "model": "gpt-6.1-sol",
		"config": map[string]any{"model_reasoning_effort": "low"},
	}, &started)
	c.await("thread/started", nil)
	effortTurn(t, c, started.Thread.ID, "", "ultrathink about this probe")
	effortTurn(t, c, started.Thread.ID, "", "plain effort probe")
	effortTurn(t, c, started.Thread.ID, "max", "explicit effort probe")
	efforts := make([]string, 0, 3)
	for _, request := range requests.seen() {
		efforts = append(efforts, request.effort)
	}
	expect(t, slices.Equal(efforts, []string{"max", "low", "max"}), "captured request efforts", efforts)
}
