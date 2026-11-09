package rpc

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestAuditTurnStartRejectsUnknownEffort(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	c.fails("turn/start", map[string]any{
		"threadId": threadID,
		"input":    []textInput{{Type: "text", Text: "hi"}},
		"effort":   "loud",
	}, codeInvalidParams, "unsupported model_reasoning_effort loud")
}
