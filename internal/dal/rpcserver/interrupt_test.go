package rpcserver

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestTurnInterruptRejectsWrongTurn(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	c.fails("turn/interrupt", map[string]any{"threadId": threadID, "turnId": "nope"},
		rpc.CodeInvalidRequest, "turn nope is not running")
}

func TestTurnInterruptRejectsMissingTurn(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	c.fails("turn/interrupt", map[string]any{"threadId": threadID},
		rpc.CodeInvalidParams, "missing turnId")
}
