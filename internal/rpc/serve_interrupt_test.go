package rpc

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestTurnInterruptRejectsWrongTurn(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	c.fails("turn/interrupt", map[string]any{"threadId": threadID, "turnId": "nope"},
		codeInvalidRequest, "turn nope is not running")
}

func TestTurnInterruptRejectsMissingTurn(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	c.fails("turn/interrupt", map[string]any{"threadId": threadID},
		codeInvalidParams, "missing turnId")
}
