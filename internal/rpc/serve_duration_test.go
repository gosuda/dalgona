package rpc

import (
	"context"
	"encoding/json/v2"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
)

func TestDurationsComeFromTheJournal(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	base := time.Date(2026, 11, 11, 10, 0, 0, 0, time.UTC)
	var ticks atomic.Int64
	r.cfg.Clock = func() time.Time { return base.Add(time.Duration(ticks.Add(1)) * 250 * time.Millisecond) }
	reopen(t, r)
	r.llm.push(llmReply{body: sseCall("call-1", "exec", `{"command":"echo hi"}`)}, llmReply{body: sseText("done")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "run it")
	wireTurnMs, wireExecMs := wireDurations(t, c.collect("turn/completed"))
	journalTurnMs, journalExecMs := journalDurations(t, r, threadID)
	if wireTurnMs <= 0 || wireExecMs <= 0 {
		t.Fatalf("durations not positive: turn %d, exec %d", wireTurnMs, wireExecMs)
	}
	if wireTurnMs != journalTurnMs {
		t.Fatalf("turn durationMs %d, journal %d", wireTurnMs, journalTurnMs)
	}
	if wireExecMs != journalExecMs {
		t.Fatalf("commandExecution durationMs %d, journal %d", wireExecMs, journalExecMs)
	}
}

func wireDurations(t *testing.T, msgs []message) (int64, int64) {
	t.Helper()
	var turnMs, execMs int64
	for _, m := range msgs {
		switch m.Method {
		case "turn/completed":
			var note struct {
				Turn struct {
					DurationMs int64 `json:"durationMs"`
				} `json:"turn"`
			}
			mustUnmarshal(t, m.Params, &note)
			turnMs = note.Turn.DurationMs
		case "item/completed":
			var note struct {
				Item struct {
					Type       string `json:"type"`
					DurationMs *int64 `json:"durationMs"`
				} `json:"item"`
			}
			mustUnmarshal(t, m.Params, &note)
			if note.Item.Type == "commandExecution" && note.Item.DurationMs != nil {
				execMs = *note.Item.DurationMs
			}
		}
	}
	return turnMs, execMs
}

func journalDurations(t *testing.T, r *rig, threadID string) (int64, int64) {
	t.Helper()
	thread, err := r.host.Resume(context.Background(), threadID)
	if err != nil {
		t.Fatal(err)
	}
	records, _, err := thread.Records()
	if err != nil {
		t.Fatal(err)
	}
	var turnBegan, turnEnded, toolBegan, toolEnded time.Time
	for _, rec := range records {
		switch value := rec.Event.(type) {
		case event.TurnStarted:
			turnBegan = value.StartedAt
		case event.TurnCompleted:
			turnEnded = value.CompletedAt
		case event.ToolStarted:
			toolBegan = value.StartedAt
		case event.ToolCompleted:
			toolEnded = value.CompletedAt
		}
	}
	return turnEnded.Sub(turnBegan).Milliseconds(), toolEnded.Sub(toolBegan).Milliseconds()
}

func mustUnmarshal(t *testing.T, raw []byte, v any) {
	t.Helper()
	if err := json.Unmarshal(raw, v); err != nil {
		t.Fatal(err)
	}
}
