package tui

import (
	json "encoding/json/v2"
	"maps"
	"slices"
	"strings"
	"testing"
)

const (
	primaryFailure = "judge-primary: decisions: http 429: judge primary unavailable"
	flaggedSummary = "Flagged: removed code"
)

var judgedMethods = []string{"dalgona/tool/judged", "dalgona/turn/judged"}

func judgedMembers(verdict, summary, by, fallback string) map[string]any {
	members := map[string]any{"verdict": verdict, "summary": summary}
	if by != "" {
		members["by"] = by
	}
	if fallback != "" {
		members["fallback"] = fallback
	}
	return members
}

func withJudges(members map[string]any, judges ...map[string]any) map[string]any {
	members["judges"] = judges
	return members
}

func judgedNotificationWith(t *testing.T, method string, members map[string]any) notificationMsg {
	t.Helper()
	params := map[string]any{"threadId": "t1", "turnId": "turn-1", "paths": []string{"a.go"}}
	if method == "dalgona/tool/judged" {
		params["itemId"] = "call_7"
	}
	maps.Copy(params, members)
	raw, err := json.Marshal(params)
	if err != nil {
		t.Fatal(err)
	}
	return notificationMsg{method: method, params: raw}
}

func assertOrdered(t *testing.T, text string, parts ...string) {
	t.Helper()
	at := 0
	for _, part := range parts {
		next := strings.Index(text[at:], part)
		if next < 0 {
			t.Fatalf("row %q does not read %q after offset %d", text, part, at)
		}
		at += next + len(part)
	}
}

func TestJudgedRowNamesAnsweringModel(t *testing.T) {
	t.Parallel()
	mixedFallback := "judge-primary: decisions: http 429"
	cases := []struct {
		name    string
		summary string
		want    string
		absent  []string
		members map[string]any
	}{
		{
			name:    "primary answered",
			summary: flaggedSummary,
			members: judgedMembers("flagged", flaggedSummary, "judge:qwen-test", ""),
			want:    "judged by qwen-test",
			absent:  []string{"fallback", "judge failed"},
		},
		{
			name:    "fallback answered",
			summary: flaggedSummary,
			members: judgedMembers("flagged", flaggedSummary, "judge:qwen-small", primaryFailure),
			want:    "judged by qwen-small (fallback from " + primaryFailure + ")",
			absent:  []string{"judged by judge-primary", "judge failed"},
		},
		{
			name:    "mixed answering models",
			summary: flaggedSummary,
			members: withJudges(judgedMembers("flagged", flaggedSummary, "judge:mixed", mixedFallback),
				map[string]any{"part": 0, "by": "judge:qwen-small", "fallback": mixedFallback},
				map[string]any{"part": 1, "by": "judge:judge-primary"},
				map[string]any{"part": 2, "by": "judge:qwen-small", "fallback": mixedFallback}),
			want:   "judged by qwen-small, judge-primary (fallback from " + mixedFallback + ")",
			absent: []string{"mixed", "judge failed"},
		},
		{
			name:    "judge failed",
			summary: "Failed: part 1 of 1: decisions: http 503",
			members: judgedMembers("error", "Failed: part 1 of 1: decisions: http 503", "judge:judge-primary", ""),
			want:    "judge failed: judge-primary",
			absent:  []string{"judged by"},
		},
		{
			name:    "no request sent",
			summary: "Failed: review: no part to judge",
			members: judgedMembers("error", "Failed: review: no part to judge", "judge", ""),
			absent:  []string{"judged by", "judge failed", "fallback"},
		},
		{
			name:    "older server without attribution",
			summary: flaggedSummary,
			members: judgedMembers("flagged", flaggedSummary, "", ""),
			absent:  []string{"judged by", "judge failed", "fallback"},
		},
	}
	for _, tc := range cases {
		for _, method := range judgedMethods {
			t.Run(tc.name+" "+method, func(t *testing.T) {
				t.Parallel()
				m := schemaStartedModel(t)
				m, _ = drive(t, m, judgedNotificationWith(t, method, tc.members))
				row := stripANSI(lastEntry(t, m))
				assertOrdered(t, row, "review", tc.summary, "a.go", tc.want)
				for _, absent := range tc.absent {
					if strings.Contains(row, absent) {
						t.Fatalf("row %q holds %q", row, absent)
					}
				}
			})
		}
	}
}

func TestJudgedAttributionAddsNoRowForQuietVerdicts(t *testing.T) {
	t.Parallel()
	cases := []struct {
		verdict, level string
		want           bool
	}{
		{"clean", "high", false},
		{"clean", "medium", false},
		{"uncertain", "medium", false},
		{"uncertain", "high", true},
		{"flagged", "medium", true},
	}
	for _, tc := range cases {
		for _, method := range judgedMethods {
			m := schemaStartedModel(t)
			m.screen.verbosity = tc.level
			before := len(m.out.entries)
			members := judgedMembers(tc.verdict, "Summary", "judge:qwen-small", primaryFailure)
			m, _ = drive(t, m, judgedNotificationWith(t, method, members))
			if got := len(m.out.entries) > before; got != tc.want {
				t.Fatalf("%s %s at %s: printed %t, want %t", method, tc.verdict, tc.level, got, tc.want)
			}
			if tc.want {
				assertContains(t, stripANSI(lastEntry(t, m)), "judged by qwen-small")
			}
		}
	}
}

func TestJudgedAttributionKeepsPathCapAndDeduplication(t *testing.T) {
	t.Parallel()
	for _, method := range judgedMethods {
		m := schemaStartedModel(t)
		members := judgedMembers("flagged", flaggedSummary, "judge:qwen-small", primaryFailure)
		members["paths"] = []string{"a.go", "b.go", "c.go", "d.go"}
		m, _ = drive(t, m, judgedNotificationWith(t, method, members))
		row := stripANSI(lastEntry(t, m))
		assertOrdered(t, row, "review", flaggedSummary, "a.go", "b.go", "c.go", "+1 more", "judged by qwen-small")
		if strings.Contains(row, "d.go") {
			t.Fatalf("%s: a fourth path is shown: %q", method, row)
		}
		before := len(m.out.entries)
		m, _ = drive(t, m, judgedNotificationWith(t, method, members))
		if len(m.out.entries) != before {
			t.Fatalf("%s: a repeated notification added a row: %d entries, want %d", method, len(m.out.entries), before)
		}
	}
}

func TestJudgedAttributionWrapsUnderTheSummary(t *testing.T) {
	t.Parallel()
	long := strings.Repeat("x", 1024)
	indent := strings.Repeat(" ", len(reviewLead))
	for _, width := range []int{40, 120} {
		m := schemaStartedModel(t)
		m.screen.width = width
		members := judgedMembers("flagged", flaggedSummary, "judge:qwen-small", "judge-primary: "+long)
		m, _ = drive(t, m, judgedNotificationWith(t, "dalgona/tool/judged", members))
		lines := strings.Split(stripANSI(lastEntry(t, m)), "\n")
		start := slices.IndexFunc(lines, func(line string) bool { return strings.HasPrefix(line, indent+"judged by") })
		if start < 1 {
			t.Fatalf("width %d: no attribution line below the summary: %q", width, lines)
		}
		if over := slices.IndexFunc(lines, func(line string) bool { return m.screen.cells(line) > width-1 }); over >= 0 {
			line := lines[over]
			t.Fatalf("width %d: line is %d cells, want at most %d: %q", width, m.screen.cells(line), width-1, line)
		}
		for _, line := range lines[start:] {
			if !strings.HasPrefix(line, indent) {
				t.Fatalf("width %d: attribution line %q lost its indent", width, line)
			}
		}
		if text := strings.ReplaceAll(strings.Join(lines[start:], ""), " ", ""); !strings.Contains(text, long) {
			t.Fatalf("width %d: the fallback text lost characters: %q", width, text)
		}
	}
}

func TestJudgedRowSanitizesAttribution(t *testing.T) {
	t.Parallel()
	m := schemaStartedModel(t)
	members := judgedMembers("flagged", flaggedSummary, "judge:qwen\x1b[2Jsmall", "judge-primary: first\nsecond\x1b]0;title\x07")
	m, _ = drive(t, m, judgedNotificationWith(t, "dalgona/tool/judged", members))
	text := lastEntry(t, m)
	for _, bad := range []string{"\x1b[2J", "\x1b]0;", "\x07"} {
		if strings.Contains(text, bad) {
			t.Fatalf("row %q holds the control sequence %q", text, bad)
		}
	}
	assertContains(t, stripANSI(text), "judged by qwensmall (fallback from judge-primary: first second)")
}
