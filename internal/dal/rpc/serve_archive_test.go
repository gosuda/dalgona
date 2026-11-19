package rpc

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func awaitThreadNote(t *testing.T, c *wireClient, method, threadID string) {
	t.Helper()
	c.await(method, func(m message) bool { return fieldOf(t, m.Params, "threadId") == threadID })
}

func TestUnarchiveNotifies(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	subscriber := r.connect()
	caller := r.connect()
	threadID := startThread(t, r, subscriber, nil)
	startTurn(t, subscriber, threadID, "first")
	subscriber.await("turn/completed", nil)

	caller.ok("thread/archive", map[string]any{"threadId": threadID}, nil)
	awaitThreadNote(t, subscriber, "thread/archived", threadID)
	awaitThreadNote(t, caller, "thread/archived", threadID)

	var unarchived ThreadReadResponse
	caller.ok("thread/unarchive", map[string]any{"threadId": threadID}, &unarchived)
	expect(t, unarchived.Thread.ID == threadID, "thread/unarchive response thread", unarchived.Thread)
	awaitThreadNote(t, subscriber, "thread/unarchived", threadID)
	awaitThreadNote(t, caller, "thread/unarchived", threadID)
}
