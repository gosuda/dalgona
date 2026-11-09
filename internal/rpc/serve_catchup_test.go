package rpc

import (
	"context"
	"encoding/json/jsontext"
	"log/slog"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestCatchUpSettlesReplayedApproval(t *testing.T) {
	r := newDalgonaRig(t)
	ctx := t.Context()
	thread, err := r.host.Start(ctx, agent.ThreadSpec{Cwd: r.cwd, Model: "openai/gpt-test"})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = thread.Close() })
	read, err := thread.ReadAfter(0)
	if err != nil {
		t.Fatal(err)
	}
	s := &server{
		host: r.host, log: slog.New(slog.DiscardHandler),
		watches: map[string]*watch{}, pending: map[string]context.CancelFunc{}, logins: map[string]context.CancelFunc{},
	}
	askCtx, cancel := context.WithCancel(ctx)
	s.pending["req-1"] = cancel
	w := &watch{thread: thread, tracker: s.newTracker(thread.ID()), head: read.Head}
	w.tracker.sink = &collector{}
	if err := thread.Append(ctx,
		event.TurnStarted{TurnID: "turn-1"},
		event.ToolStarted{CallID: "call-1", TurnID: "turn-1", Tool: "exec", Class: "exec", Args: jsontext.Value("{}")},
		event.ApprovalRequested{RequestID: "req-1", CallID: "call-1", Summary: "run it"},
		event.ApprovalResolved{RequestID: "req-1", Decision: "accept", By: "client"},
	); err != nil {
		t.Fatal(err)
	}
	if err := s.catchUp(w); err != nil {
		t.Fatal(err)
	}
	if w.head != read.Head+4 {
		t.Fatalf("head=%d, want %d", w.head, read.Head+4)
	}
	if _, live := s.pending["req-1"]; live {
		t.Fatal("replayed approval.resolved left the pending server request unsettled")
	}
	if askCtx.Err() == nil {
		t.Fatal("replayed approval.resolved did not cancel the pending ask")
	}
}
