package event_test

import (
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

func TestTurnJudgedValidation(t *testing.T) {
	t.Parallel()
	valid := event.TurnJudged{
		TurnID: "turn-3", By: "judge", Verdict: "flagged", Paths: []string{"a.go"},
		Scores: map[string]float64{"sound": 0.2, "incomplete": 0.94}, Flags: []string{"incomplete"},
		Spoken: []string{"incomplete"}, Feedback: true, ElapsedMS: 1,
	}
	if err := valid.Validate(); err != nil {
		t.Fatalf("valid turn.judged rejected: %v", err)
	}
	tests := []struct {
		name string
		edit func(*event.TurnJudged)
	}{
		{"spoken not in flags", func(e *event.TurnJudged) { e.Spoken = []string{"lost_code"} }},
		{"feedback true with empty spoken", func(e *event.TurnJudged) { e.Spoken = nil }},
		{"error with scores", func(e *event.TurnJudged) { e.Verdict, e.Error = "error", "boom" }},
		{"skipped not in paths", func(e *event.TurnJudged) { e.Skipped = []string{"b.go"} }},
		{"uncertain with flags", func(e *event.TurnJudged) { e.Verdict, e.Feedback, e.Spoken = "uncertain", false, nil }},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			e := valid
			e.Paths = slices.Clone(valid.Paths)
			e.Scores = map[string]float64{"sound": 0.2, "incomplete": 0.94}
			e.Flags = slices.Clone(valid.Flags)
			e.Spoken = slices.Clone(valid.Spoken)
			tc.edit(&e)
			if err := e.Validate(); err == nil {
				t.Fatalf("Validate accepted %+v, want an error", e)
			}
		})
	}
}
