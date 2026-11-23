package rpcserver

import (
	"log/slog"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func newEffortTracker() *tracker {
	return newTracker("t1", nil, nil, nil, slog.New(slog.DiscardHandler))
}

func requireEmptyEffortResolution(t *testing.T, tr *tracker, out *judgedSink, methods int) {
	t.Helper()
	if tr.turnEffort == nil || *tr.turnEffort != "" || len(out.methods) != methods {
		t.Fatalf("empty effort resolution = %+v, got %d methods, want %d", tr.turnEffort, len(out.methods), methods)
	}
}

func requireEffortWirePayload(t *testing.T, out *judgedSink) {
	t.Helper()
	raw, err := rpc.Marshal(out.notes[0]())
	if err != nil {
		t.Fatal(err)
	}
	if err := validateDalgona(t, "dalgona.turn.effort.json", raw); err != nil {
		t.Fatalf("empty effort notification %s: %v", raw, err)
	}
	for _, member := range []string{"threadId", "turnId", "effort"} {
		mutateDalgona(t, "dalgona.turn.effort.json", raw, member)
	}
}

func requireEffortReplaySilent(t *testing.T, tr *tracker, seq uint64, ev event.Event) {
	t.Helper()
	live := &judgedSink{}
	tr.sink = live
	tr.apply(seq, ev)
	if len(live.methods) != 0 {
		t.Fatal("replay resolution was published again as live")
	}
}

func requireEffortReplayLifecycle(t *testing.T, replay replayer, effort string) {
	t.Helper()
	reconstructed := newEffortTracker()
	replay(reconstructed, []event.Seqd{
		{Seq: 1, Event: event.TurnStarted{TurnID: "active"}},
		{Seq: 2, Event: event.TurnEffort{TurnID: "active", Effort: effort}},
	})
	if reconstructed.turn != "active" || reconstructed.turnEffort == nil || *reconstructed.turnEffort != effort {
		t.Fatalf("replay lost resolved effort %q: %+v", effort, reconstructed)
	}
	requireEffortReplaySilent(t, reconstructed, 3, event.TurnEffort{TurnID: "active", Effort: effort})
	reconstructed.apply(4, event.TurnCompleted{TurnID: "active", Status: "interrupted"})
	if reconstructed.turnEffort != nil {
		t.Fatal("interrupted replay retained effort")
	}
}

func TestTrackerEffortLiveAndReplayLifecycle(t *testing.T) {
	t.Parallel()
	tr := newEffortTracker()
	out := &judgedSink{}
	tr.sink = out
	tr.beginTurn(event.TurnStarted{TurnID: "turn-1"})
	tr.apply(1, event.TurnEffort{TurnID: "old", Effort: "max"})
	if tr.turnEffort != nil || len(out.methods) != 0 {
		t.Fatal("stale effort changed the active turn")
	}
	tr.apply(2, event.TurnEffort{TurnID: "turn-1", Effort: ""})
	tr.apply(3, event.TurnEffort{TurnID: "turn-1", Effort: ""})
	requireEmptyEffortResolution(t, tr, out, 1)
	requireEffortWirePayload(t, out)
	tr.endTurn()
	if tr.turnEffort != nil {
		t.Fatal("completion retained a resolved effort")
	}
	tr.beginTurn(event.TurnStarted{TurnID: "turn-2"})
	tr.apply(4, event.TurnEffort{TurnID: "turn-2", Effort: ""})
	if len(out.methods) != 2 {
		t.Fatal("the next turn did not publish its first empty resolution")
	}
	for _, effort := range []string{"max", ""} {
		for _, replay := range []replayer{(*tracker).replayAll, (*tracker).replayLive} {
			requireEffortReplayLifecycle(t, replay, effort)
		}
	}
}

func TestTrackerTakesItsKeywordSourceAtConstruction(t *testing.T) {
	source := func(threadID, text string, kind agent.InputKind) []agent.KeywordMatch {
		if threadID != "t1" || kind != agent.InputPrompt {
			return nil
		}
		return []agent.KeywordMatch{{Word: "probe", Start: 0, End: 5, Summary: "notice added"}}
	}
	tr := newTracker("t1", nil, nil, source, slog.New(slog.DiscardHandler))
	message := event.UserMessage{ItemID: "u1", TurnID: "turn-1", Text: "probe", Notices: []event.Notice{{Word: "probe"}}}
	matches := tr.noticeMatches(message, "probe")
	if len(matches) != 1 || matches[0].Word != "probe" || matches[0].ByteRange.End != 5 {
		t.Fatalf("noticeMatches = %+v, want the probe match from the source given at construction", matches)
	}
}
