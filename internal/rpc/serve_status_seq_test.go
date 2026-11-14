package rpc

import (
	"encoding/json/v2"
	"log/slog"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

type statusSink struct{ notes []func() writer }

func (s *statusSink) turn(string, Turn)                    {}
func (s *statusSink) item(string, string, *Item)           {}
func (s *statusSink) notify(_ string, build func() writer) { s.notes = append(s.notes, build) }

func TestTurnStartedStatusCarriesTrackerSeq(t *testing.T) {
	sink := &statusSink{}
	tr := newTracker("t1", nil, nil, nil, slog.New(slog.DiscardHandler))
	tr.sink = sink
	tr.apply(1, event.TurnStarted{TurnID: "u1"})
	if len(sink.notes) == 0 {
		t.Fatal("turn start emitted no status")
	}
	for i, build := range sink.notes {
		raw, err := encode(build())
		if err != nil {
			t.Fatal(err)
		}
		var status struct {
			Seq uint64 `json:"seq"`
		}
		if err := json.Unmarshal(raw, &status); err != nil {
			t.Fatalf("decode thread/status/changed %s: %v", raw, err)
		}
		if want := uint64(i + 1); status.Seq != want {
			t.Fatalf("status %d seq = %d, want the tracker seq %d as the top-level member", i, status.Seq, want)
		}
	}
}
