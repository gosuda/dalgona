package event_test

import (
	"encoding/json/jsontext"
	"reflect"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

const flaggedToolJSON = `{"type":"tool.judged","call_id":"call_7","turn_id":"turn-3","by":"judge:typesafe/jev-1.14","verdict":"flagged","paths":["internal/model/retry.go","web/src/lib/Retry.svelte","config/retry.toml"],"scores":{"broken_syntax":0.02,"layout_motion":0.04,"lost_code":0.94,"narration":0.05,"needless_layer":0.03,"off_task":0.11,"off_token":0.02,"placeholder":0.03,"sound":0.31,"unclear_contract":0.06,"unguarded_state":0.12,"unsafe_input":0.01,"wasted_work":0.04,"wrong_logic":0.62},"flags":["lost_code"],"spoken":["lost_code"],"feedback":true,"parts":2,"elapsed_ms":1840}`

const flaggedTurnJSON = `{"type":"turn.judged","turn_id":"turn-3","by":"judge:typesafe/jev-1.14","verdict":"flagged","paths":["internal/model/retry.go","web/src/lib/Retry.svelte","config/retry.toml"],"scores":{"incomplete":0.93,"sound":0.31},"flags":["incomplete"],"spoken":["incomplete"],"feedback":true,"skipped":["config/retry.toml"],"elapsed_ms":2650}`

func flaggedToolJudged() event.ToolJudged {
	return event.ToolJudged{
		CallID: "call_7", TurnID: "turn-3", By: "judge:typesafe/jev-1.14", Verdict: "flagged",
		Paths: []string{"internal/model/retry.go", "web/src/lib/Retry.svelte", "config/retry.toml"},
		Scores: map[string]float64{
			"broken_syntax": 0.02, "layout_motion": 0.04, "lost_code": 0.94, "narration": 0.05,
			"needless_layer": 0.03, "off_task": 0.11, "off_token": 0.02, "placeholder": 0.03, "sound": 0.31,
			"unclear_contract": 0.06, "unguarded_state": 0.12, "unsafe_input": 0.01, "wasted_work": 0.04,
			"wrong_logic": 0.62,
		},
		Flags: []string{"lost_code"}, Spoken: []string{"lost_code"}, Feedback: true,
		Parts: 2, ElapsedMS: 1840,
	}
}

func flaggedTurnJudged() event.TurnJudged {
	return event.TurnJudged{
		TurnID: "turn-3", By: "judge:typesafe/jev-1.14", Verdict: "flagged",
		Paths:    []string{"internal/model/retry.go", "web/src/lib/Retry.svelte", "config/retry.toml"},
		Scores:   map[string]float64{"incomplete": 0.93, "sound": 0.31},
		Flags:    []string{"incomplete"},
		Spoken:   []string{"incomplete"},
		Feedback: true, Skipped: []string{"config/retry.toml"}, ElapsedMS: 2650,
	}
}

func TestToolJudgedRoundTripAndValidation(t *testing.T) {
	t.Parallel()
	encoded, err := event.Encode(flaggedToolJudged())
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	if string(encoded) != flaggedToolJSON {
		t.Fatalf("journal form\ngot:  %s\nwant: %s", encoded, flaggedToolJSON)
	}
	decoded, err := event.Decode([]byte(flaggedToolJSON))
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if !reflect.DeepEqual(decoded, flaggedToolJudged()) {
		t.Fatalf("decoded = %#v, want %#v", decoded, flaggedToolJudged())
	}
	tests := []struct {
		name   string
		mutate func(*event.ToolJudged)
	}{
		{"empty call_id", func(j *event.ToolJudged) { j.CallID = "" }},
		{"empty turn_id", func(j *event.ToolJudged) { j.TurnID = "" }},
		{"empty by", func(j *event.ToolJudged) { j.By = "" }},
		{"unknown verdict", func(j *event.ToolJudged) { j.Verdict = "ok" }},
		{"no paths", func(j *event.ToolJudged) { j.Paths = nil }},
		{"65 paths", func(j *event.ToolJudged) { j.Paths = make([]string, 65) }},
		{"empty path", func(j *event.ToolJudged) { j.Paths = []string{""} }},
		{"negative elapsed", func(j *event.ToolJudged) { j.ElapsedMS = -1 }},
		{"score above one", func(j *event.ToolJudged) { j.Scores["sound"] = 1.5 }},
		{"flag not scored", func(j *event.ToolJudged) { j.Flags = []string{"missing"}; j.Spoken = []string{"missing"} }},
		{"repeated flag", func(j *event.ToolJudged) { j.Flags = []string{"lost_code", "lost_code"} }},
		{"clean with flags", func(j *event.ToolJudged) { j.Verdict = "clean" }},
		{"flagged without flags", func(j *event.ToolJudged) { j.Flags = nil; j.Spoken = nil; j.Feedback = false }},
		{"uncertain with flags", func(j *event.ToolJudged) { j.Verdict = "uncertain" }},
		{"clean without scores", func(j *event.ToolJudged) {
			j.Verdict, j.Scores, j.Flags, j.Spoken, j.Feedback = "clean", nil, nil, nil, false
		}},
		{"error with scores", func(j *event.ToolJudged) { j.Verdict, j.Error = "error", "review timed out" }},
		{"error without text", func(j *event.ToolJudged) {
			j.Verdict, j.Scores, j.Flags, j.Spoken, j.Feedback = "error", nil, nil, nil, false
		}},
		{"error over one KiB", func(j *event.ToolJudged) {
			j.Verdict, j.Scores, j.Flags, j.Spoken, j.Feedback = "error", nil, nil, nil, false
			j.Error = strings.Repeat("x", 1025)
		}},
		{"spoken not a flag", func(j *event.ToolJudged) { j.Spoken = []string{"placeholder"} }},
		{"feedback without spoken", func(j *event.ToolJudged) { j.Spoken = nil }},
		{"spoken without feedback", func(j *event.ToolJudged) { j.Feedback = false }},
		{"feedback on clean", func(j *event.ToolJudged) {
			j.Verdict, j.Flags, j.Spoken = "clean", nil, nil
		}},
		{"zero parts", func(j *event.ToolJudged) { j.Parts = 0 }},
		{"failed parts above parts", func(j *event.ToolJudged) { j.FailedParts = 3 }},
		{"negative failed parts", func(j *event.ToolJudged) { j.FailedParts = -1 }},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			judged := flaggedToolJudged()
			tc.mutate(&judged)
			if err := judged.Validate(); err == nil {
				t.Fatal("Validate accepted an invalid tool.judged")
			}
		})
	}
}

func TestToolJudgedPartsRoundTrip(t *testing.T) {
	t.Parallel()
	threeParts := flaggedToolJudged()
	threeParts.Parts = 3
	assertRoundTrip(t, 0, threeParts)
	encoded, err := event.Encode(threeParts)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	if !strings.Contains(string(encoded), `"parts":3`) {
		t.Fatalf("encoded %s has no parts member", encoded)
	}
	onePart := flaggedToolJudged()
	onePart.Parts = 1
	encoded, err = event.Encode(onePart)
	if err != nil {
		t.Fatalf("encode one part: %v", err)
	}
	if strings.Contains(string(encoded), `"parts"`) {
		t.Fatalf("one part must omit parts: %s", encoded)
	}
	withFailed := flaggedToolJudged()
	withFailed.Parts, withFailed.FailedParts = 3, 1
	assertRoundTrip(t, 0, withFailed)
	encoded, err = event.Encode(withFailed)
	if err != nil {
		t.Fatalf("encode failed parts: %v", err)
	}
	if !strings.Contains(string(encoded), `"parts":3,"failed_parts":1`) {
		t.Fatalf("encoded %s lacks parts then failed_parts", encoded)
	}
	rejects := []string{
		strings.Replace(flaggedToolJSON, `"parts":2`, `"parts":1`, 1),
		strings.Replace(flaggedToolJSON, `"parts":2`, `"parts":2,"failed_parts":0`, 1),
	}
	for _, raw := range rejects {
		if _, err := event.Decode([]byte(raw)); err == nil {
			t.Fatalf("Decode accepted %s", raw)
		}
	}
}

func TestTurnJudgedRoundTripAndValidation(t *testing.T) {
	t.Parallel()
	encoded, err := event.Encode(flaggedTurnJudged())
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	if string(encoded) != flaggedTurnJSON {
		t.Fatalf("journal form\ngot:  %s\nwant: %s", encoded, flaggedTurnJSON)
	}
	decoded, err := event.Decode([]byte(flaggedTurnJSON))
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if !reflect.DeepEqual(decoded, flaggedTurnJudged()) {
		t.Fatalf("decoded = %#v, want %#v", decoded, flaggedTurnJudged())
	}
	tests := []struct {
		name   string
		mutate func(*event.TurnJudged)
	}{
		{"empty turn_id", func(j *event.TurnJudged) { j.TurnID = "" }},
		{"empty by", func(j *event.TurnJudged) { j.By = "" }},
		{"unknown verdict", func(j *event.TurnJudged) { j.Verdict = "ok" }},
		{"no paths", func(j *event.TurnJudged) { j.Paths = nil }},
		{"65 paths", func(j *event.TurnJudged) { j.Paths = make([]string, 65) }},
		{"empty path", func(j *event.TurnJudged) { j.Paths = []string{""} }},
		{"negative elapsed", func(j *event.TurnJudged) { j.ElapsedMS = -1 }},
		{"score above one", func(j *event.TurnJudged) { j.Scores["sound"] = 1.5 }},
		{"flag not scored", func(j *event.TurnJudged) { j.Flags = []string{"missing"}; j.Spoken = []string{"missing"} }},
		{"repeated flag", func(j *event.TurnJudged) { j.Flags = []string{"incomplete", "incomplete"} }},
		{"clean with flags", func(j *event.TurnJudged) { j.Verdict = "clean" }},
		{"flagged without flags", func(j *event.TurnJudged) { j.Flags = nil; j.Spoken = nil; j.Feedback = false }},
		{"uncertain with flags", func(j *event.TurnJudged) { j.Verdict = "uncertain" }},
		{"clean without scores", func(j *event.TurnJudged) {
			j.Verdict, j.Scores, j.Flags, j.Spoken, j.Feedback = "clean", nil, nil, nil, false
		}},
		{"error with scores", func(j *event.TurnJudged) { j.Verdict, j.Error = "error", "review timed out" }},
		{"error without text", func(j *event.TurnJudged) {
			j.Verdict, j.Scores, j.Flags, j.Spoken, j.Feedback = "error", nil, nil, nil, false
		}},
		{"error over one KiB", func(j *event.TurnJudged) {
			j.Verdict, j.Scores, j.Flags, j.Spoken, j.Feedback = "error", nil, nil, nil, false
			j.Error = strings.Repeat("x", 1025)
		}},
		{"spoken not a flag", func(j *event.TurnJudged) { j.Spoken = []string{"sound"} }},
		{"unknown score name", func(j *event.TurnJudged) { j.Scores["lost_code"] = 0.2 }},
		{"flag not incomplete", func(j *event.TurnJudged) {
			j.Scores["sound"] = 0.9
			j.Flags = []string{"sound"}
			j.Spoken = []string{"sound"}
		}},
		{"feedback without spoken", func(j *event.TurnJudged) { j.Spoken = nil }},
		{"spoken without feedback", func(j *event.TurnJudged) { j.Feedback = false }},
		{"feedback on clean", func(j *event.TurnJudged) {
			j.Verdict, j.Flags, j.Spoken = "clean", nil, nil
		}},
		{"skipped not a judged path", func(j *event.TurnJudged) { j.Skipped = []string{"other.go"} }},
		{"repeated skipped path", func(j *event.TurnJudged) { j.Skipped = []string{"config/retry.toml", "config/retry.toml"} }},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			judged := flaggedTurnJudged()
			tc.mutate(&judged)
			if err := judged.Validate(); err == nil {
				t.Fatal("Validate accepted an invalid turn.judged")
			}
		})
	}
}

func foldJudgedEvents(events ...event.Event) (event.State, error) {
	var state event.State
	for i, e := range events {
		next, err := event.Fold(state, uint64(i+1), e)
		if err != nil {
			return next, err
		}
		state = next
	}
	return state, nil
}

func judgedCall(turn, call string) event.ToolJudged {
	return event.ToolJudged{
		CallID: call, TurnID: turn, By: "judge", Verdict: "clean", Paths: []string{"a.go"},
		Scores: map[string]float64{"sound": 0.02}, Parts: 1, ElapsedMS: 5,
	}
}

func TestFoldToolJudged(t *testing.T) {
	t.Parallel()
	open := event.TurnStarted{TurnID: "t1"}
	start := event.ToolStarted{CallID: "c1", TurnID: "t1", Tool: "patch", Class: "edit", Args: jsontext.Value(`{}`)}
	done := event.ToolCompleted{CallID: "c1", OK: true, Recovered: "none"}
	end := event.TurnCompleted{TurnID: "t1", Status: "completed"}
	call := event.AssistantMessage{
		ItemID: "a1", TurnID: "t1", StopReason: "tool_calls",
		ToolCalls: []event.ToolCall{{ID: "c1", Name: "patch", Args: jsontext.Value(`{}`)}},
	}
	tests := []struct {
		name   string
		events []event.Event
		err    string
	}{
		{"call in flight", []event.Event{open, start, judgedCall("t1", "c1")}, "event: fold: tool.judged before tool.completed: c1"},
		{"call pending", []event.Event{open, call, judgedCall("t1", "c1")}, "event: fold: tool.judged before tool.completed: c1"},
		{"call completed", []event.Event{open, start, done, judgedCall("t1", "c1")}, ""},
		{"repeat for one call", []event.Event{open, start, done, judgedCall("t1", "c1"), judgedCall("t1", "c1")}, "event: fold: duplicate tool.judged c1"},
		{"turn not open", []event.Event{open, start, done, judgedCall("t2", "c1")}, "event: fold: event turn t2 does not match open turn t1"},
		{"call id repeats in a later turn", []event.Event{
			open, start, done, judgedCall("t1", "c1"), end,
			event.TurnStarted{TurnID: "t2"},
			event.ToolStarted{CallID: "c1", TurnID: "t2", Tool: "patch", Class: "edit", Args: jsontext.Value(`{}`)},
			event.ToolCompleted{CallID: "c1", OK: true, Recovered: "none"},
			judgedCall("t2", "c1"),
		}, ""},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			_, err := foldJudgedEvents(tc.events...)
			checkFoldError(t, err, tc.err)
		})
	}
}

func checkFoldError(t *testing.T, err error, want string) {
	t.Helper()
	if want == "" {
		if err != nil {
			t.Fatalf("fold error = %v, want none", err)
		}
		return
	}
	if err == nil || !strings.Contains(err.Error(), want) {
		t.Fatalf("fold error = %v, want %q", err, want)
	}
}

func TestFoldTurnJudged(t *testing.T) {
	t.Parallel()
	open := event.TurnStarted{TurnID: "t1"}
	judged := event.TurnJudged{TurnID: "t1", By: "judge", Verdict: "clean", Paths: []string{"a.go"}, Scores: map[string]float64{"sound": 0.02}, ElapsedMS: 5}
	end := event.TurnCompleted{TurnID: "t1", Status: "completed"}
	tests := []struct {
		name   string
		events []event.Event
		err    string
	}{
		{"open turn", []event.Event{open, judged}, ""},
		{"second turn.judged for the turn", []event.Event{open, judged, judged}, "event: fold: duplicate turn.judged t1"},
		{"turn not open", []event.Event{judged}, "event: fold: event turn t1 does not match open turn "},
		{"turn already completed", []event.Event{open, end, judged}, "event: fold: event turn t1 does not match open turn "},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			_, err := foldJudgedEvents(tc.events...)
			checkFoldError(t, err, tc.err)
		})
	}
}
