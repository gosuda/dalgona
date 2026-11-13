package rpc

import (
	"log/slog"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

type judgedSink struct {
	methods []string
	notes   []func() writer
}

func (s *judgedSink) turn(string, Turn)          {}
func (s *judgedSink) item(string, string, *Item) {}
func (s *judgedSink) notify(method string, build func() writer) {
	s.methods = append(s.methods, method)
	s.notes = append(s.notes, build)
}

func requireJudgedNotification(t *testing.T, sink *judgedSink, method, want string) {
	t.Helper()
	if len(sink.methods) != 1 || sink.methods[0] != method {
		t.Fatalf("notifications = %v, want one %s", sink.methods, method)
	}
	o := newOut()
	sink.notes[0]().write(o)
	raw, err := o.bytes()
	if err != nil {
		t.Fatal(err)
	}
	if string(raw) != want {
		t.Fatalf("payload = %s, want %s", raw, want)
	}
}

func newJudgedTracker(sink *judgedSink) *tracker {
	tr := newTracker("thr_1", nil, nil, nil, slog.New(slog.DiscardHandler))
	tr.sink = sink
	return tr
}

func TestTrackerNotifiesToolJudged(t *testing.T) {
	sink := &judgedSink{}
	tr := newJudgedTracker(sink)
	tr.apply(1, event.ToolJudged{
		CallID: "call_7", TurnID: "turn-3", By: "judge", Verdict: "flagged",
		Paths: []string{"internal/model/retry.go"}, Scores: map[string]float64{"lost_code": 0.94, "broken_syntax": 0.02},
		Flags: []string{"lost_code"},
	})
	requireJudgedNotification(t, sink, "dalgona/tool/judged", `{"threadId":"thr_1","turnId":"turn-3","itemId":"call_7","verdict":"flagged","summary":"Flagged: removed code (not sent to the model)","paths":["internal/model/retry.go"]}`)
}

func TestTrackerNotifiesCleanVerdict(t *testing.T) {
	sink := &judgedSink{}
	tr := newJudgedTracker(sink)
	tr.apply(1, event.ToolJudged{
		CallID: "call_7", TurnID: "turn-3", By: "judge", Verdict: "clean",
		Paths: []string{"internal/model/retry.go"}, Scores: map[string]float64{"lost_code": 0.02},
	})
	requireJudgedNotification(t, sink, "dalgona/tool/judged", `{"threadId":"thr_1","turnId":"turn-3","itemId":"call_7","verdict":"clean","summary":"No problems found","paths":["internal/model/retry.go"]}`)
}

func TestTrackerNotifiesTurnJudged(t *testing.T) {
	sink := &judgedSink{}
	tr := newJudgedTracker(sink)
	tr.apply(1, event.TurnJudged{
		TurnID: "turn-3", By: "judge", Verdict: "uncertain",
		Paths: []string{"internal/model/retry.go"}, Scores: map[string]float64{"incomplete": 0.62, "sound": 0.9},
	})
	requireJudgedNotification(t, sink, "dalgona/turn/judged", `{"threadId":"thr_1","turnId":"turn-3","verdict":"uncertain","summary":"Uncertain: incomplete","paths":["internal/model/retry.go"]}`)
}
