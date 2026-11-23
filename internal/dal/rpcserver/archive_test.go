package rpcserver

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"

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

	var unarchived rpc.ThreadReadResponse
	caller.ok("thread/unarchive", map[string]any{"threadId": threadID}, &unarchived)
	expect(t, unarchived.Thread.ID == threadID, "thread/unarchive response thread", unarchived.Thread)
	awaitThreadNote(t, subscriber, "thread/unarchived", threadID)
	awaitThreadNote(t, caller, "thread/unarchived", threadID)
}

func countNotes(t *testing.T, notes []message, method, threadID string) int {
	t.Helper()
	var n int
	for _, m := range notes {
		if m.Method == method && fieldOf(t, m.Params, "threadId") == threadID {
			n++
		}
	}
	return n
}

func TestArchiveNotifiesAWatchingCallerOnce(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	watcher := r.connect()
	threadID := startThread(t, r, watcher, nil)
	startTurn(t, watcher, threadID, "first")
	watcher.await("turn/completed", nil)

	watcher.ok("thread/archive", map[string]any{"threadId": threadID}, nil)
	watcher.ok("thread/unarchive", map[string]any{"threadId": threadID}, nil)
	startTurn(t, watcher, threadID, "second")
	notes := watcher.collect()
	for _, method := range []string{"thread/archived", "thread/unarchived"} {
		if got := countNotes(t, notes, method, threadID); got != 1 {
			t.Fatalf("%s notifications to the watching caller = %d, want 1", method, got)
		}
	}
}
