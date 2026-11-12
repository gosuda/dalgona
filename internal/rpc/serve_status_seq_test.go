package rpc

import (
	"log/slog"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

type statusSink struct{ notes []func() writer }

func (s *statusSink) turn(string, Turn)                    {}
func (s *statusSink) item(string, string, *Item)           {}
func (s *statusSink) notify(_ string, build func() writer) { s.notes = append(s.notes, build) }

func TestStatusNotificationAlwaysCarriesSeq(t *testing.T) {
	sink := &statusSink{}
	tr := newTracker("t1", nil, nil, slog.New(slog.DiscardHandler))
	tr.sink = sink
	tr.apply(1, event.TurnStarted{TurnID: "u1"})
	if len(sink.notes) == 0 {
		t.Fatal("turn start emitted no status")
	}
	o := newOut()
	sink.notes[0]().write(o)
	raw, err := o.bytes()
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(raw), `"seq":1`) {
		t.Fatalf("status = %s, want the tracker seq 1 as a member", raw)
	}
}
