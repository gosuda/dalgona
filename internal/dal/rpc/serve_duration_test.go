package rpc

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"log/slog"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
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
	wireTurnMs, wireExecMs := wireDurations(t, c.collect())
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

type durationSink struct {
	turns []Turn
	items []Item
}

func (s *durationSink) turn(_ string, turn Turn) { s.turns = append(s.turns, turn) }
func (s *durationSink) item(method, _ string, it *Item) {
	if method == methodItemCompleted {
		s.items = append(s.items, *it)
	}
}
func (s *durationSink) notify(string, func() writer) {}

func TestRecordsWithoutTimesOmitDurations(t *testing.T) {
	sink := &durationSink{}
	tr := newTracker("t1", nil, nil, nil, slog.New(slog.DiscardHandler))
	tr.sink = sink
	tr.apply(1, event.TurnStarted{TurnID: "turn-1"})
	tr.apply(2, event.ToolStarted{CallID: "c1", TurnID: "turn-1", Tool: "exec", Class: "exec", Args: jsontext.Value(`{"command":"ls"}`)})
	tr.apply(3, event.ToolCompleted{CallID: "c1", OK: true})
	tr.apply(4, event.TurnCompleted{TurnID: "turn-1", Status: statusCompleted})
	if len(sink.turns) != 2 || len(sink.items) != 1 {
		t.Fatalf("turns = %+v items = %+v", sink.turns, sink.items)
	}
	encoded := newOut()
	done := sink.turns[1]
	done.write(encoded)
	raw, err := encoded.bytes()
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(raw), "durationMs") {
		t.Fatalf("turn/completed without times carries durationMs: %s", raw)
	}
	if sink.items[0].DurationMs != nil {
		t.Fatalf("command without journal times has durationMs %d", *sink.items[0].DurationMs)
	}

	timed := &durationSink{}
	tr2 := newTracker("t1", nil, nil, nil, slog.New(slog.DiscardHandler))
	tr2.sink = timed
	begin := time.Date(2026, 11, 11, 10, 0, 0, 0, time.UTC)
	tr2.apply(1, event.TurnStarted{TurnID: "turn-1", StartedAt: begin})
	tr2.apply(2, event.ToolStarted{CallID: "c1", TurnID: "turn-1", Tool: "exec", Class: "exec", StartedAt: begin, Args: jsontext.Value(`{"command":"ls"}`)})
	tr2.apply(3, event.ToolCompleted{CallID: "c1", OK: true, CompletedAt: begin.Add(2 * time.Second)})
	tr2.apply(4, event.TurnCompleted{TurnID: "turn-1", Status: statusCompleted, CompletedAt: begin.Add(3 * time.Second)})
	if timed.turns[1].DurationMs != 3000 {
		t.Fatalf("turn durationMs = %d, want 3000", timed.turns[1].DurationMs)
	}
	if timed.items[0].DurationMs == nil || *timed.items[0].DurationMs != 2000 {
		t.Fatalf("item durationMs = %+v, want 2000", timed.items[0].DurationMs)
	}
}
