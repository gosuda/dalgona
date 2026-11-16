package rpc

import (
	"encoding/json/v2"
	"log/slog"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
)

type statusSink struct{ notes []func() writer }

func (s *statusSink) turn(string, Turn)                    {}
func (s *statusSink) item(string, string, *Item)           {}
func (s *statusSink) notify(_ string, build func() writer) { s.notes = append(s.notes, build) }

func TestTurnStartedStatusCarriesTrackerSeq(t *testing.T) {
	sink := &statusSink{}
	tr := newTracker("t1", nil, nil, nil, slog.New(slog.DiscardHandler))
	tr.sink = sink
	tr.apply(7, event.TurnStarted{TurnID: "u1"})
	if len(sink.notes) != 1 {
		t.Fatalf("expected 1 status note, got %d", len(sink.notes))
	}
	statusSeq := trackerStatusSequence(t, sink.notes[0])
	if statusSeq != 7 {
		t.Fatalf("status 0 seq = %d, want tracker event seq 7", statusSeq)
	}

	tr.apply(7, event.TurnStarted{TurnID: "u2"})
	if len(sink.notes) != 2 {
		t.Fatalf("expected 2 status notes, got %d", len(sink.notes))
	}
	statusSeq = trackerStatusSequence(t, sink.notes[1])
	if statusSeq != 8 {
		t.Fatalf("status 1 seq = %d, want monotonically bumped seq 8", statusSeq)
	}

	tr.apply(3, event.TurnStarted{TurnID: "u3"})
	if len(sink.notes) != 3 {
		t.Fatalf("expected 3 status notes, got %d", len(sink.notes))
	}
	statusSeq = trackerStatusSequence(t, sink.notes[2])
	if statusSeq != 9 {
		t.Fatalf("status 2 seq = %d, want monotonically bumped seq 9", statusSeq)
	}
}

func trackerStatusSequence(t *testing.T, note func() writer) uint64 {
	t.Helper()
	var status struct {
		Seq uint64 `json:"seq"`
	}
	raw, err := encode(note())
	if err != nil {
		t.Fatal(err)
	}
	if err = json.Unmarshal(raw, &status); err != nil {
		t.Fatalf("decode thread/status/changed %s: %v", raw, err)
	}
	return status.Seq
}

func TestServeWireTurnStartedStatusCarriesEventSeq(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	turnID := startTurn(t, c, threadID, "check status seq")
	notes := c.collect()
	notes = append(notes, c.await("thread/status/changed", func(m message) bool {
		return wireThreadIdle(t, m, threadID)
	}))

	thread, err := r.host.Resume(t.Context(), threadID)
	if err != nil {
		t.Fatal(err)
	}
	records, _, err := thread.Records()
	if err != nil {
		t.Fatal(err)
	}

	var turnStartSeq, turnCompleteSeq uint64
	for _, rec := range records {
		switch ev := rec.Event.(type) {
		case event.TurnStarted:
			if ev.TurnID == turnID {
				turnStartSeq = rec.Seq
			}
		case event.TurnCompleted:
			if ev.TurnID == turnID {
				turnCompleteSeq = rec.Seq
			}
		}
	}
	requireTurnRecordPresence(t, turnStartSeq, turnCompleteSeq)

	activeSeq, idleSeq := wireStatusSequences(t, notes, threadID)
	if activeSeq != turnStartSeq {
		t.Fatalf("active status notification seq %d != turn start record seq %d", activeSeq, turnStartSeq)
	}
	requireWireCompletionSequence(t, activeSeq, idleSeq, turnCompleteSeq)
}

func requireTurnRecordPresence(t *testing.T, turnStartSeq, turnCompleteSeq uint64) {
	t.Helper()
	if turnStartSeq == 0 || turnCompleteSeq == 0 {
		t.Fatalf("turn records missing: start %d, complete %d", turnStartSeq, turnCompleteSeq)
	}
}

func requireWireCompletionSequence(t *testing.T, activeSeq, idleSeq, turnCompleteSeq uint64) {
	t.Helper()
	if idleSeq != turnCompleteSeq {
		t.Fatalf("idle status notification seq %d != turn complete record seq %d", idleSeq, turnCompleteSeq)
	}
	if activeSeq >= idleSeq {
		t.Fatalf("active status seq %d must be strictly less than idle status seq %d", activeSeq, idleSeq)
	}
}

func wireThreadIdle(t *testing.T, m message, threadID string) bool {
	t.Helper()
	var notification struct {
		ThreadID string `json:"threadId"`
		Status   struct {
			Type string `json:"type"`
		} `json:"status"`
	}
	if err := json.Unmarshal(m.Params, &notification); err != nil {
		t.Fatalf("decode thread/status/changed %s: %v", m.Params, err)
	}
	return notification.ThreadID == threadID && notification.Status.Type == "idle"
}

func wireStatusSequences(t *testing.T, notes []message, threadID string) (uint64, uint64) {
	t.Helper()
	var activeSeq, idleSeq uint64
	var foundActive, foundIdle bool
	for _, m := range notes {
		if m.Method != "thread/status/changed" {
			continue
		}
		var notification struct {
			ThreadID string `json:"threadId"`
			Status   struct {
				Type string `json:"type"`
			} `json:"status"`
			Seq uint64 `json:"seq"`
		}
		if err := json.Unmarshal(m.Params, &notification); err != nil {
			t.Fatalf("decode thread/status/changed %s: %v", m.Params, err)
		}
		if notification.ThreadID != threadID {
			continue
		}
		switch notification.Status.Type {
		case "active":
			if !foundActive {
				activeSeq = notification.Seq
				foundActive = true
			}
		case "idle":
			idleSeq = notification.Seq
			foundIdle = true
		}
	}

	requireWireStatusPresence(t, foundActive, foundIdle, notes)
	return activeSeq, idleSeq
}

func requireWireStatusPresence(t *testing.T, foundActive, foundIdle bool, notes []message) {
	t.Helper()
	if !foundActive || !foundIdle {
		t.Fatalf("missing status notifications: active=%v, idle=%v in notes %+v", foundActive, foundIdle, notes)
	}
}
