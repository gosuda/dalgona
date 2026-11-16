package event_test

import (
	"reflect"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

const mixedToolJSON = `{"type":"tool.judged","call_id":"call_7","turn_id":"turn-3","by":"judge:mixed","fallback":"typesafe/jev-1.14: decisions: http 429: rate limited","judges":[{"part":0,"by":"judge:qwen-small","fallback":"typesafe/jev-1.14: decisions: http 429: rate limited"},{"part":1,"by":"judge:typesafe/jev-1.14"}],"verdict":"flagged","paths":["a.go"],"scores":{"lost_code":0.94,"sound":0.31},"flags":["lost_code"],"spoken":["lost_code"],"feedback":true,"parts":2,"elapsed_ms":1840}`

const fallbackTurnJSON = `{"type":"turn.judged","turn_id":"turn-3","by":"judge:qwen-small","fallback":"judge-primary: decisions: http 402: payment required","verdict":"clean","paths":["a.go"],"scores":{"incomplete":0.02,"sound":0.9},"elapsed_ms":2650}`

func mixedToolJudged() event.ToolJudged {
	return event.ToolJudged{
		CallID: "call_7", TurnID: "turn-3", By: "judge:mixed",
		Fallback: "typesafe/jev-1.14: decisions: http 429: rate limited",
		Judges: []event.JudgeAttribution{
			{Part: 0, By: "judge:qwen-small", Fallback: "typesafe/jev-1.14: decisions: http 429: rate limited"},
			{Part: 1, By: "judge:typesafe/jev-1.14"},
		},
		Verdict: "flagged", Paths: []string{"a.go"},
		Scores: map[string]float64{"lost_code": 0.94, "sound": 0.31},
		Flags:  []string{"lost_code"}, Spoken: []string{"lost_code"}, Feedback: true,
		Parts: 2, ElapsedMS: 1840,
	}
}

func fallbackTurnJudged() event.TurnJudged {
	return event.TurnJudged{
		TurnID: "turn-3", By: "judge:qwen-small", Fallback: "judge-primary: decisions: http 402: payment required",
		Verdict: "clean", Paths: []string{"a.go"},
		Scores: map[string]float64{"incomplete": 0.02, "sound": 0.9}, ElapsedMS: 2650,
	}
}

func TestToolJudgedAttributionJournalForm(t *testing.T) {
	t.Parallel()
	encoded, err := event.Encode(mixedToolJudged())
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	if string(encoded) != mixedToolJSON {
		t.Fatalf("journal form\ngot:  %s\nwant: %s", encoded, mixedToolJSON)
	}
	decoded, err := event.Decode([]byte(mixedToolJSON))
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if !reflect.DeepEqual(decoded, mixedToolJudged()) {
		t.Fatalf("decoded = %#v, want %#v", decoded, mixedToolJudged())
	}
	plain, err := event.Encode(flaggedToolJudged())
	if err != nil {
		t.Fatalf("encode without attribution: %v", err)
	}
	if strings.Contains(string(plain), `"fallback"`) || strings.Contains(string(plain), `"judges"`) {
		t.Fatalf("a judgment without attribution must write neither member: %s", plain)
	}
}

func TestTurnJudgedFallbackJournalForm(t *testing.T) {
	t.Parallel()
	encoded, err := event.Encode(fallbackTurnJudged())
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	if string(encoded) != fallbackTurnJSON {
		t.Fatalf("journal form\ngot:  %s\nwant: %s", encoded, fallbackTurnJSON)
	}
	decoded, err := event.Decode([]byte(fallbackTurnJSON))
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if !reflect.DeepEqual(decoded, fallbackTurnJudged()) {
		t.Fatalf("decoded = %#v, want %#v", decoded, fallbackTurnJudged())
	}
	plain, err := event.Encode(flaggedTurnJudged())
	if err != nil {
		t.Fatalf("encode without attribution: %v", err)
	}
	if strings.Contains(string(plain), `"fallback"`) || strings.Contains(string(plain), `"judges"`) {
		t.Fatalf("a judgment without attribution must write neither member: %s", plain)
	}
}

func TestJudgedRecordsWithoutAttributionStillDecodeAndFold(t *testing.T) {
	t.Parallel()
	tool, turn := decodeUnattributedJudged(t)
	if tool.Fallback != "" || tool.Judges != nil || tool.By != "judge:typesafe/jev-1.14" {
		t.Fatalf("older tool.judged = %#v, want its judge and no fallback or judges", tool)
	}
	if turn.Fallback != "" || turn.Judges != nil {
		t.Fatalf("older turn.judged = %#v, want no fallback or judges", turn)
	}
	open := event.TurnStarted{TurnID: "turn-3"}
	start := event.ToolStarted{CallID: "call_7", TurnID: "turn-3", Tool: "patch", Class: "edit", Args: []byte(`{}`)}
	done := event.ToolCompleted{CallID: "call_7", OK: true, Recovered: "none"}
	if err := foldJudgedEvents(open, start, done, tool, turn); err != nil {
		t.Fatalf("fold older judged records: %v", err)
	}
	if err := foldJudgedEvents(open, start, done, mixedToolJudged(), fallbackTurnJudged()); err != nil {
		t.Fatalf("fold attributed judged records: %v", err)
	}
}

func decodeUnattributedJudged(t *testing.T) (event.ToolJudged, event.TurnJudged) {
	t.Helper()
	tool, err := event.Decode([]byte(flaggedToolJSON))
	if err != nil {
		t.Fatalf("decode older tool.judged: %v", err)
	}
	turn, err := event.Decode([]byte(flaggedTurnJSON))
	if err != nil {
		t.Fatalf("decode older turn.judged: %v", err)
	}
	return tool.(event.ToolJudged), turn.(event.TurnJudged)
}

func TestToolJudgedAttributionValidation(t *testing.T) {
	t.Parallel()
	long := strings.Repeat("x", event.MaxJudgeErrorBytes+1)
	tests := []struct {
		name   string
		mutate func(*event.ToolJudged)
	}{
		{"one judge entry", func(j *event.ToolJudged) { j.Parts, j.Judges = 1, j.Judges[:1] }},
		{"judges for one part", func(j *event.ToolJudged) { j.Parts = 1 }},
		{"fewer judges than parts", func(j *event.ToolJudged) { j.Parts = 3 }},
		{"part index out of order", func(j *event.ToolJudged) { j.Judges[0].Part, j.Judges[1].Part = 1, 0 }},
		{"part index repeats", func(j *event.ToolJudged) { j.Judges[1].Part = 0 }},
		{"judge without by", func(j *event.ToolJudged) { j.Judges[1].By = "" }},
		{"judge fallback over one KiB", func(j *event.ToolJudged) { j.Judges[0].Fallback = long }},
		{"fallback over one KiB", func(j *event.ToolJudged) { j.Fallback = long }},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			judged := mixedToolJudged()
			tc.mutate(&judged)
			if err := judged.Validate(); err == nil {
				t.Fatal("Validate accepted an invalid attribution")
			}
		})
	}
	t.Run("fallback of one KiB accepted", func(t *testing.T) {
		t.Parallel()
		judged := mixedToolJudged()
		judged.Fallback = strings.Repeat("x", event.MaxJudgeErrorBytes)
		if err := judged.Validate(); err != nil {
			t.Fatalf("Validate rejected a fallback of %d bytes: %v", event.MaxJudgeErrorBytes, err)
		}
	})
	t.Run("attribution on an error verdict accepted", func(t *testing.T) {
		t.Parallel()
		judged := mixedToolJudged()
		judged.Verdict, judged.Scores, judged.Flags, judged.Spoken, judged.Feedback = "error", nil, nil, nil, false
		judged.Error, judged.FailedParts = "part 1 of 2: boom", 2
		if err := judged.Validate(); err != nil {
			t.Fatalf("Validate rejected an error verdict with judges: %v", err)
		}
	})
}

func TestTurnJudgedAttributionValidation(t *testing.T) {
	t.Parallel()
	long := strings.Repeat("x", event.MaxJudgeErrorBytes+1)
	tests := []struct {
		name   string
		mutate func(*event.TurnJudged)
	}{
		{"fallback over one KiB", func(j *event.TurnJudged) { j.Fallback = long }},
		{"one judge entry", func(j *event.TurnJudged) {
			j.Judges = []event.JudgeAttribution{{Part: 0, By: "judge:qwen-small"}}
		}},
		{"judge without by", func(j *event.TurnJudged) {
			j.Judges = []event.JudgeAttribution{{Part: 0, By: "judge:a"}, {Part: 1}}
		}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			judged := fallbackTurnJudged()
			tc.mutate(&judged)
			if err := judged.Validate(); err == nil {
				t.Fatal("Validate accepted an invalid attribution")
			}
		})
	}
	t.Run("two judge entries accepted", func(t *testing.T) {
		t.Parallel()
		judged := fallbackTurnJudged()
		judged.Judges = []event.JudgeAttribution{{Part: 0, By: "judge:a"}, {Part: 1, By: "judge:b", Fallback: "a: down"}}
		if err := judged.Validate(); err != nil {
			t.Fatalf("Validate rejected two judge entries: %v", err)
		}
	})
}

func TestJudgedAttributionDecodeRejectsMalformedMembers(t *testing.T) {
	t.Parallel()
	rejects := map[string]string{
		"judges without part": strings.Replace(mixedToolJSON, `{"part":1,"by":"judge:typesafe/jev-1.14"}`, `{"by":"judge:typesafe/jev-1.14"}`, 1),
		"judges without by":   strings.Replace(mixedToolJSON, `{"part":1,"by":"judge:typesafe/jev-1.14"}`, `{"part":1}`, 1),
		"judges not an array": strings.Replace(mixedToolJSON, `"judges":[{"part":0,"by":"judge:qwen-small","fallback":"typesafe/jev-1.14: decisions: http 429: rate limited"},{"part":1,"by":"judge:typesafe/jev-1.14"}]`, `"judges":"none"`, 1),
		"fallback not text":   strings.Replace(fallbackTurnJSON, `"fallback":"judge-primary: decisions: http 402: payment required"`, `"fallback":7`, 1),
	}
	for name, raw := range rejects {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if raw == mixedToolJSON || raw == fallbackTurnJSON {
				t.Fatal("the mutation did not change the record")
			}
			if _, err := event.Decode([]byte(raw)); err == nil {
				t.Fatalf("Decode accepted %s", raw)
			}
		})
	}
}
