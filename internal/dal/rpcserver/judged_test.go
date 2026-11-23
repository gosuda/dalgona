package rpcserver

import (
	json "encoding/json/v2"
	"log/slog"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

type judgedSink struct {
	methods []string
	notes   []func() rpc.Encoder
}

func (s *judgedSink) turn(string, rpc.Turn) {}

func (s *judgedSink) item(string, string, *rpc.Item) {}

func (s *judgedSink) notify(method string, build func() rpc.Encoder) {
	s.methods = append(s.methods, method)
	s.notes = append(s.notes, build)
}

type judgedFields struct {
	turnID, itemID, verdict, summaryPrefix string
	paths                                  []string
	by, fallback                           string
	judges                                 []rpc.JudgeAttribution
}

func requireJudgedNotification(t *testing.T, sink *judgedSink, method, schema string, want judgedFields) {
	t.Helper()
	if len(sink.methods) != 1 || sink.methods[0] != method {
		t.Fatalf("notifications = %v, want one %s", sink.methods, method)
	}
	payload := decodeJudgedPayload(t, sink, schema)
	requireJudgedIdentity(t, payload, want)
	requireJudgedSummary(t, payload, want)
}

type judgedPayload struct {
	ThreadID string                 `json:"threadId"`
	TurnID   string                 `json:"turnId"`
	ItemID   string                 `json:"itemId"`
	Verdict  string                 `json:"verdict"`
	Summary  string                 `json:"summary"`
	Paths    []string               `json:"paths"`
	By       string                 `json:"by"`
	Fallback string                 `json:"fallback"`
	Judges   []rpc.JudgeAttribution `json:"judges"`
}

func decodeJudgedPayload(t *testing.T, sink *judgedSink, schema string) judgedPayload {
	t.Helper()
	o := rpc.NewWriter()
	sink.notes[0]().Encode(o)
	raw, err := o.Bytes()
	if err != nil {
		t.Fatal(err)
	}
	if err := validateDalgona(t, schema, raw); err != nil {
		t.Fatalf("payload %s violates %s: %v", raw, schema, err)
	}
	var payload judgedPayload
	if err := json.Unmarshal(raw, &payload); err != nil {
		t.Fatalf("decode %s: %v", raw, err)
	}
	return payload
}

func requireJudgedIdentity(t *testing.T, payload judgedPayload, want judgedFields) {
	t.Helper()
	if payload.ThreadID != "thr_1" || payload.TurnID != want.turnID || payload.ItemID != want.itemID || payload.Verdict != want.verdict || !slices.Equal(payload.Paths, want.paths) {
		t.Fatalf("payload = %+v, want the ids, verdict and paths of the event", payload)
	}
	if payload.By != want.by || payload.Fallback != want.fallback || !slices.Equal(payload.Judges, want.judges) {
		t.Fatalf("payload = %+v, want by %q, fallback %q and judges %+v", payload, want.by, want.fallback, want.judges)
	}
}

func requireJudgedSummary(t *testing.T, payload judgedPayload, want judgedFields) {
	t.Helper()
	if want.summaryPrefix != "" && !strings.HasPrefix(payload.Summary, want.summaryPrefix) {
		t.Fatalf("summary = %q, want the prefix %q", payload.Summary, want.summaryPrefix)
	}
	if want.summaryPrefix == "" && payload.Summary == "" {
		t.Fatalf("summary is empty, want a summary sentence")
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
	requireJudgedNotification(t, sink, "dalgona/tool/judged", "dalgona.tool.judged.json", judgedFields{
		turnID: "turn-3", itemID: "call_7", verdict: "flagged", by: "judge",
		summaryPrefix: "Flagged: removed code", paths: []string{"internal/model/retry.go"},
	})
	o := rpc.NewWriter()
	sink.notes[0]().Encode(o)
	raw, err := o.Bytes()
	if err != nil {
		t.Fatal(err)
	}
	var value map[string]any
	err = json.Unmarshal(raw, &value)
	if err != nil {
		t.Fatal(err)
	}
	if value == nil {
		value = map[string]any{}
	}
	value["extra"] = float64(1)
	var extended []byte
	extended, err = json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	if err := validateDalgona(t, "dalgona.tool.judged.json", extended); err == nil {
		t.Fatal("a payload with an extra member validates, want schema rejection")
	}
}

func TestTrackerNotifiesCleanVerdict(t *testing.T) {
	sink := &judgedSink{}
	tr := newJudgedTracker(sink)
	tr.apply(1, event.ToolJudged{
		CallID: "call_7", TurnID: "turn-3", By: "judge", Verdict: "clean",
		Paths: []string{"internal/model/retry.go"}, Scores: map[string]float64{"lost_code": 0.02},
	})
	requireJudgedNotification(t, sink, "dalgona/tool/judged", "dalgona.tool.judged.json", judgedFields{
		turnID: "turn-3", itemID: "call_7", verdict: "clean", by: "judge",
		paths: []string{"internal/model/retry.go"},
	})
}

func TestTrackerNotifiesTurnJudged(t *testing.T) {
	sink := &judgedSink{}
	tr := newJudgedTracker(sink)
	tr.apply(1, event.TurnJudged{
		TurnID: "turn-3", By: "judge", Verdict: "uncertain",
		Paths: []string{"internal/model/retry.go"}, Scores: map[string]float64{"incomplete": 0.62, "sound": 0.9},
	})
	requireJudgedNotification(t, sink, "dalgona/turn/judged", "dalgona.turn.judged.json", judgedFields{
		turnID: "turn-3", verdict: "uncertain", summaryPrefix: "Uncertain: incomplete", by: "judge",
		paths: []string{"internal/model/retry.go"},
	})
}

func judgedMembers(t *testing.T, sink *judgedSink) map[string]any {
	t.Helper()
	o := rpc.NewWriter()
	sink.notes[0]().Encode(o)
	raw, err := o.Bytes()
	if err != nil {
		t.Fatal(err)
	}
	var members map[string]any
	if err := json.Unmarshal(raw, &members); err != nil {
		t.Fatalf("decode %s: %v", raw, err)
	}
	return members
}

func TestTrackerNotifiesFallbackAttribution(t *testing.T) {
	const cause = "judge-primary: decisions: http 429: rate limited"
	t.Run("tool judgment with partitioned attribution", func(t *testing.T) {
		sink := &judgedSink{}
		newJudgedTracker(sink).apply(1, event.ToolJudged{
			CallID: "call_7", TurnID: "turn-3", By: "judge:mixed", Fallback: cause, Verdict: "clean",
			Judges: []event.JudgeAttribution{
				{Part: 0, By: "judge:qwen-small", Fallback: cause},
				{Part: 1, By: "judge:judge-primary"},
			},
			Paths: []string{"a.go"}, Scores: map[string]float64{"sound": 0.9}, Parts: 2,
		})
		requireJudgedNotification(t, sink, "dalgona/tool/judged", "dalgona.tool.judged.json", judgedFields{
			turnID: "turn-3", itemID: "call_7", verdict: "clean", by: "judge:mixed", fallback: cause,
			paths: []string{"a.go"},
			judges: []rpc.JudgeAttribution{
				{Part: 0, By: "judge:qwen-small", Fallback: cause},
				{Part: 1, By: "judge:judge-primary"},
			},
		})
	})
	t.Run("turn judgment with a fallback answer", func(t *testing.T) {
		sink := &judgedSink{}
		newJudgedTracker(sink).apply(1, event.TurnJudged{
			TurnID: "turn-3", By: "judge:qwen-small", Fallback: cause, Verdict: "clean",
			Paths: []string{"a.go"}, Scores: map[string]float64{"incomplete": 0.02, "sound": 0.9},
		})
		requireJudgedNotification(t, sink, "dalgona/turn/judged", "dalgona.turn.judged.json", judgedFields{
			turnID: "turn-3", verdict: "clean", by: "judge:qwen-small", fallback: cause, paths: []string{"a.go"},
		})
	})
	t.Run("a primary answer writes neither member", func(t *testing.T) {
		sink := &judgedSink{}
		newJudgedTracker(sink).apply(1, event.ToolJudged{
			CallID: "call_7", TurnID: "turn-3", By: "judge:judge-primary", Verdict: "clean",
			Paths: []string{"a.go"}, Scores: map[string]float64{"sound": 0.9},
		})
		members := judgedMembers(t, sink)
		if _, ok := members["fallback"]; ok {
			t.Fatalf("payload %v holds fallback, want it absent", members)
		}
		if _, ok := members["judges"]; ok {
			t.Fatalf("payload %v holds judges, want it absent", members)
		}
		if members["by"] != "judge:judge-primary" {
			t.Fatalf("payload %v, want by judge:judge-primary", members)
		}
	})
}

func TestJournalReplayCarriesAttributionToTheWire(t *testing.T) {
	const cause = "judge-primary: decisions: http 402: payment required"
	original := event.ToolJudged{
		CallID: "call_7", TurnID: "turn-3", By: "judge:mixed", Fallback: cause, Verdict: "clean",
		Judges: []event.JudgeAttribution{
			{Part: 0, By: "judge:qwen-small", Fallback: cause},
			{Part: 1, By: "judge:judge-primary"},
		},
		Paths: []string{"a.go"}, Scores: map[string]float64{"sound": 0.9}, Parts: 2,
	}
	journaled, err := event.Encode(original)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	replayed, err := event.Decode(journaled)
	if err != nil {
		t.Fatalf("decode %s: %v", journaled, err)
	}
	sink := &judgedSink{}
	newJudgedTracker(sink).apply(1, replayed)
	requireJudgedNotification(t, sink, "dalgona/tool/judged", "dalgona.tool.judged.json", judgedFields{
		turnID: "turn-3", itemID: "call_7", verdict: "clean", by: "judge:mixed", fallback: cause,
		paths: []string{"a.go"},
		judges: []rpc.JudgeAttribution{
			{Part: 0, By: "judge:qwen-small", Fallback: cause},
			{Part: 1, By: "judge:judge-primary"},
		},
	})
}
