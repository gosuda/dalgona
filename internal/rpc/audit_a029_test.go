package rpc

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestAuditInterruptRejectsWrongTurn(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	c.fails("turn/interrupt", map[string]any{"threadId": threadID, "turnId": "nope"},
		codeInvalidRequest, "turn nope is not running")
}

func TestAuditInterruptRejectsMissingTurn(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	c.fails("turn/interrupt", map[string]any{"threadId": threadID},
		codeInvalidParams, "missing turnId")
}
