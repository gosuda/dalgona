package rpc

import (
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
)

func TestSnapshotKeepsHistoryAcrossCompaction(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	r.llm.push(llmReply{body: sseText("done")})
	turnID := startTurn(t, c, threadID, "hello")
	c.collect()
	compactInsideTurn(t, r, threadID, turnID)
	assertSnapshotTurns(t, c, r, threadID, turnID)
}

func compactInsideTurn(t *testing.T, r *rig, threadID, turnID string) {
	t.Helper()
	thread, err := r.host.Resume(t.Context(), threadID)
	if err != nil {
		t.Fatal(err)
	}
	records, _, err := thread.Records()
	if err != nil {
		t.Fatal(err)
	}
	started := turnStartSeq(t, records, turnID)
	marker := event.Compaction{FirstKeptSeq: started + 1, Summary: "summary", Method: "manual"}
	if err := thread.Append(t.Context(), marker); err != nil {
		t.Fatal(err)
	}
}

func turnStartSeq(t *testing.T, records []event.Seqd, turnID string) uint64 {
	t.Helper()
	for _, item := range records {
		if value, ok := item.Event.(event.TurnStarted); ok && value.TurnID == turnID {
			return item.Seq
		}
	}
	t.Fatal("no TurnStarted in records")
	return 0
}

func assertSnapshotTurns(t *testing.T, c *wireClient, r *rig, threadID, turnID string) {
	t.Helper()
	var read ThreadReadResponse
	c.ok("thread/read", map[string]any{"threadId": threadID, "includeTurns": true}, &read)
	if read.Thread.Cwd != r.cwd {
		t.Fatalf("thread/read cwd = %q, want %q", read.Thread.Cwd, r.cwd)
	}
	if len(read.Thread.Turns) != 1 || read.Thread.Turns[0].ID != turnID {
		t.Fatalf("thread/read turns = %+v, want one turn %s", read.Thread.Turns, turnID)
	}
	var resumed ThreadResumeResponse
	c.ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	if resumed.Cwd != r.cwd {
		t.Fatalf("thread/resume cwd = %q, want %q", resumed.Cwd, r.cwd)
	}
	if len(resumed.Thread.Turns) != 1 || resumed.Thread.Turns[0].ID != turnID {
		t.Fatalf("thread/resume turns = %+v, want one turn %s", resumed.Thread.Turns, turnID)
	}
}

func TestSnapshotKeepsSessionCreatedAfterRevertToRoot(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	r.llm.push(llmReply{body: sseText("done")})
	turnID := startTurn(t, c, threadID, "hello")
	c.collect()
	c.ok("thread/revert", map[string]any{"threadId": threadID, "beforeTurnId": turnID}, nil)
	c.await("thread/reverted", nil)
	var read ThreadReadResponse
	c.ok("thread/read", map[string]any{"threadId": threadID, "includeTurns": true}, &read)
	if read.Thread.Cwd != r.cwd {
		t.Fatalf("thread/read cwd = %q, want %q", read.Thread.Cwd, r.cwd)
	}
	if read.Thread.Model != "openai/gpt-test" {
		t.Fatalf("thread/read model = %q, want %q", read.Thread.Model, "openai/gpt-test")
	}
	if read.Thread.CreatedAt == 0 {
		t.Fatalf("thread/read createdAt = 0, want non-zero")
	}
	if len(read.Thread.Turns) != 0 {
		t.Fatalf("thread/read turns = %+v, want zero turns", read.Thread.Turns)
	}
	var resumed ThreadResumeResponse
	c.ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	if resumed.Cwd != r.cwd {
		t.Fatalf("thread/resume cwd = %q, want %q", resumed.Cwd, r.cwd)
	}
	if resumed.Model != "openai/gpt-test" {
		t.Fatalf("thread/resume model = %q, want %q", resumed.Model, "openai/gpt-test")
	}
	if resumed.Thread.CreatedAt != read.Thread.CreatedAt {
		t.Fatalf("thread/resume createdAt = %d, want %d", resumed.Thread.CreatedAt, read.Thread.CreatedAt)
	}
	if len(resumed.Thread.Turns) != 0 {
		t.Fatalf("thread/resume turns = %+v, want zero turns", resumed.Thread.Turns)
	}
}
