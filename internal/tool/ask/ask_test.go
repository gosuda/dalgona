package ask

import (
	"reflect"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool"
)

func repeatRune(r rune, n int) string {
	return strings.Repeat(string(r), n)
}

func TestParseAcceptsContractShapes(t *testing.T) {
	raw := []byte(`{"questions":[` +
		`{"header":"Database","question":"Which database should the service use?","options":[` +
		`{"label":"SQLite","description":"One file, no server to run."},` +
		`{"label":"Postgres","description":"A server with replication.","preview":"services:\n  db:\n    image: postgres:17"}]},` +
		`{"header":"Features","question":"Which features ship first?","multi_select":true,"options":[` +
		`{"label":"Search","description":"Full-text search."},` +
		`{"label":"Export","description":"CSV export."},` +
		`{"label":"Sync","description":"Two-way sync."}]},` +
		`{"header":"Name","question":"What should the service be called?"}]}`)

	got, err := Parse(raw)
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	want := []Question{
		{
			Header: "Database",
			Text:   "Which database should the service use?",
			Options: []Option{
				{Label: "SQLite", Description: "One file, no server to run."},
				{Label: "Postgres", Description: "A server with replication.", Preview: "services:\n  db:\n    image: postgres:17"},
			},
		},
		{
			Header:      "Features",
			Text:        "Which features ship first?",
			MultiSelect: true,
			Options: []Option{
				{Label: "Search", Description: "Full-text search."},
				{Label: "Export", Description: "CSV export."},
				{Label: "Sync", Description: "Two-way sync."},
			},
		},
		{Header: "Name", Text: "What should the service be called?"},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("Parse = %#v, want %#v", got, want)
	}
}

func TestParseRejectsEachLimit(t *testing.T) {
	cases := []struct {
		name string
		raw  string
		want string
	}{
		{"no questions", `{"questions":[]}`, "ask: questions needs 1 to 4 items, got 0"},
		{"five questions", `{"questions":[` + strings.Repeat(`{"header":"H","question":"Q?"},`, 4) + `{"header":"H","question":"Q?"}]}`, "ask: questions needs 1 to 4 items, got 5"},
		{"empty header", `{"questions":[{"header":"","question":"Q?"}]}`, "ask: question 1: header is empty"},
		{"long header", `{"questions":[{"header":"` + repeatRune('h', 31) + `","question":"Q?"}]}`, "ask: question 1: header is longer than 30 characters"},
		{"empty question", `{"questions":[{"header":"H","question":""}]}`, "ask: question 1: question is empty"},
		{"long question", `{"questions":[{"header":"H","question":"` + repeatRune('q', 501) + `"}]}`, "ask: question 1: question is longer than 500 characters"},
		{"one option", `{"questions":[{"header":"H","question":"Q?","options":[{"label":"A","description":"a"}]}]}`, "ask: question 1: options needs 2 to 6 items, or none for a free-text question, got 1"},
		{"seven options", `{"questions":[{"header":"H","question":"Q?","options":[` + strings.Repeat(`{"label":"A","description":"a"},`, 6) + `{"label":"B","description":"b"}]}]}`, "ask: question 1: options needs 2 to 6 items, or none for a free-text question, got 7"},
		{"multi select without options", `{"questions":[{"header":"H","question":"Q?","multi_select":true}]}`, "ask: question 1: multi_select needs options"},
		{"empty label", `{"questions":[{"header":"H","question":"Q?","options":[{"label":"A","description":"a"},{"label":"","description":"b"}]}]}`, "ask: question 1 option 2: label is empty"},
		{"long label", `{"questions":[{"header":"H","question":"Q?","options":[{"label":"A","description":"a"},{"label":"` + repeatRune('l', 81) + `","description":"b"}]}]}`, "ask: question 1 option 2: label is longer than 80 characters"},
		{"empty description", `{"questions":[{"header":"H","question":"Q?","options":[{"label":"A","description":"a"},{"label":"B","description":""}]}]}`, "ask: question 1 option 2: description is empty"},
		{"long description", `{"questions":[{"header":"H","question":"Q?","options":[{"label":"A","description":"a"},{"label":"B","description":"` + repeatRune('d', 301) + `"}]}]}`, "ask: question 1 option 2: description is longer than 300 characters"},
		{"preview of 41 lines", `{"questions":[{"header":"H","question":"Q?","options":[{"label":"A","description":"a","preview":"` + strings.Repeat(`x\n`, 40) + `x"},{"label":"B","description":"b"}]}]}`, "ask: question 1 option 1: preview is longer than 2000 characters or 40 lines"},
		{"preview of 2001 characters", `{"questions":[{"header":"H","question":"Q?","options":[{"label":"A","description":"a","preview":"` + repeatRune('p', 2001) + `"},{"label":"B","description":"b"}]}]}`, "ask: question 1 option 1: preview is longer than 2000 characters or 40 lines"},
		{"repeated label", `{"questions":[{"header":"H","question":"Q?","options":[{"label":"Postgres","description":"a"},{"label":"postgres","description":"b"}]}]}`, `ask: question 1 option 2: label "postgres" repeats option 1`},
		{"other label", `{"questions":[{"header":"H","question":"Q?","options":[{"label":"A","description":"a"},{"label":"Other","description":"b"}]}]}`, "ask: question 1 option 2: the user always gets an Other row; remove this option"},
		{"repeated question", `{"questions":[{"header":"H","question":"Which?"},{"header":"H","question":"which?"}]}`, "ask: question 2 repeats question 1"},
		{"unknown question field", `{"questions":[{"header":"H","question":"Q?","color":"red"}]}`, `ask: unknown field "color"`},
		{"unknown option field", `{"questions":[{"header":"H","question":"Q?","options":[{"label":"A","description":"a","color":"red"},{"label":"B","description":"b"}]}]}`, `ask: unknown field "color"`},
		{"unknown top-level field", `{"questions":[{"header":"H","question":"Q?"}],"extra":1}`, `ask: unknown field "extra"`},
		{"wrong type", `{"questions":"nope"}`, "ask: arguments do not match the schema:"},
		{"top-level null", `null`, "ask: arguments do not match the schema:"},
		{"questions null", `{"questions":null}`, "ask: arguments do not match the schema:"},
		{"question item null", `{"questions":[null]}`, "ask: arguments do not match the schema:"},
		{"options null", `{"questions":[{"header":"H","question":"Q?","options":null}]}`, "ask: arguments do not match the schema:"},
		{"option item null", `{"questions":[{"header":"H","question":"Q?","options":[null,{"label":"B","description":"b"}]}]}`, "ask: arguments do not match the schema:"},
		{"multi_select null", `{"questions":[{"header":"H","question":"Q?","multi_select":null}]}`, "ask: arguments do not match the schema:"},
		{"header null", `{"questions":[{"header":null,"question":"Q?"}]}`, "ask: arguments do not match the schema:"},
		{"trailing data", `{"questions":[{"header":"H","question":"Q?"}]} x`, "ask: arguments do not match the schema:"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := Parse([]byte(tc.raw))
			if err == nil {
				t.Fatalf("Parse accepted %s", tc.raw)
			}
			if strings.HasSuffix(tc.want, ":") {
				if !strings.HasPrefix(err.Error(), tc.want) {
					t.Fatalf("error = %q, want prefix %q", err.Error(), tc.want)
				}
				return
			}
			if err.Error() != tc.want {
				t.Fatalf("error = %q, want %q", err.Error(), tc.want)
			}
		})
	}
}

func TestCleanStripsTerminalControls(t *testing.T) {
	cases := []struct {
		name      string
		in        string
		multiline bool
		want      string
	}{
		{"csi color", "\x1b[31mDB\x1b[0m", false, "DB"},
		{"osc title", "\x1b]0;title\x07x", false, "x"},
		{"bidi override", "a\u202Eb", false, "ab"},
		{"c1 control", "\u009bq", false, "q"},
		{"invalid utf8 replaced", "bad\xffbyte", false, "bad\uFFFDbyte"},
		{"apc with st terminator", "\x1b_app\x1b\\y", false, "y"},
		{"escape before multibyte rune", "a\x1b\u00e9b", false, "ab"},
		{"newline folds to space", "a\nb", false, "a b"},
		{"newline kept when multiline", "a\nb", true, "a\nb"},
		{"tab becomes four spaces", "\tx", true, "    x"},
		{"single line trimmed", "  x  ", false, "x"},
		{"multiline drops blank edges", "\n\n  a\n  \n", true, "  a"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := CleanLine(tc.in)
			if tc.multiline {
				got = CleanBlock(tc.in)
			}
			if got != tc.want {
				t.Fatalf("clean(%q, multiline=%v) = %q, want %q", tc.in, tc.multiline, got, tc.want)
			}
		})
	}
}

func TestAnswersNormalizes(t *testing.T) {
	questions := []Question{
		{Header: "Database", Text: "Which?", Options: []Option{{Label: "SQLite"}, {Label: "Postgres"}}},
		{Header: "Features", Text: "Which features?", MultiSelect: true, Options: []Option{{Label: "Search"}, {Label: "Export"}, {Label: "Sync"}}},
		{Header: "Name", Text: "Name?"},
		{Header: "Pair", Text: "Pair?", MultiSelect: true, Options: []Option{{Label: "A"}, {Label: "B"}}},
	}
	byID := map[string][]string{
		"q1": {"sqlite", "Postgres"},
		"q2": {"Export", "Search", "Search", "", "  Mongo  "},
		"q3": {"first", "second"},
		"q4": {"x1", "x2", "x3", "x4", "x5"},
		"q9": {"ignored"},
	}
	got := Answers(questions, byID)
	want := [][]string{
		{"Postgres"},
		{"Search", "Export", "Mongo"},
		{"first"},
		{"x1", "x2", "x3"},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("Answers = %#v, want %#v", got, want)
	}
}

func TestAnswersCapsLengthAndAllowsNoAnswer(t *testing.T) {
	questions := []Question{{Header: "Name", Text: "Name?"}, {Header: "Other", Text: "Other?"}}
	byID := map[string][]string{"q1": {repeatRune('a', 2500)}}
	got := Answers(questions, byID)
	if n := len([]rune(got[0][0])); n != MaxAnswer {
		t.Fatalf("answer length = %d runes, want %d", n, MaxAnswer)
	}
	if len(got[1]) != 0 {
		t.Fatalf("question without an answer = %#v, want none", got[1])
	}
}

func TestFormatTexts(t *testing.T) {
	questions := []Question{
		{Header: "Database", Text: "Which database should the service use?", Options: []Option{{Label: "SQLite"}, {Label: "Postgres"}}},
		{Header: "Features", Text: "Which features ship first?", MultiSelect: true, Options: []Option{{Label: "Search"}, {Label: "Export"}, {Label: "Sync"}}},
		{Header: "Name", Text: "What should the service be called?"},
	}
	cases := []struct {
		name   string
		reply  Reply
		output string
		ok     bool
	}{
		{
			name:   "answered",
			reply:  Reply{Outcome: OutcomeAnswered, Answers: [][]string{{"Postgres"}, {"Search", "Export"}, {"ledger-svc"}}},
			output: "User answers:\n- Database (Which database should the service use?): Postgres\n- Features (Which features ship first?): Search; Export\n- Name (What should the service be called?): \"ledger-svc\"",
			ok:     true,
		},
		{
			name:   "typed other text",
			reply:  Reply{Outcome: OutcomeAnswered, Answers: [][]string{{"Mongo"}, nil, nil}},
			output: "User answers:\n- Database (Which database should the service use?): Other: \"Mongo\"\n- Features (Which features ship first?): no answer\n- Name (What should the service be called?): no answer",
			ok:     true,
		},
		{
			name:   "every list empty is skipped",
			reply:  Reply{Outcome: OutcomeAnswered, Answers: [][]string{nil, nil, nil}},
			output: TextSkipped,
			ok:     true,
		},
		{name: "cancelled", reply: Reply{Outcome: OutcomeCancelled}, output: TextCancelled, ok: true},
		{name: "no client", reply: Reply{Outcome: OutcomeNoClient}, output: TextNoClient, ok: false},
		{name: "interrupted", reply: Reply{Outcome: OutcomeInterrupted}, output: TextInterrupted, ok: false},
		{name: "failed", reply: Reply{Outcome: OutcomeFailed, Detail: "boom"}, output: "declined: boom", ok: false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := Format(questions, tc.reply)
			want := tool.Result{Output: []byte(tc.output), OK: tc.ok}
			if string(got.Output) != string(want.Output) || got.OK != want.OK {
				t.Fatalf("Format = %q ok=%v, want %q ok=%v", got.Output, got.OK, want.Output, want.OK)
			}
		})
	}
}

func TestFormatCleansInvalidUTF8Answer(t *testing.T) {
	questions := []Question{{Header: "H", Text: "Q"}}
	reply := Reply{Outcome: OutcomeAnswered, Answers: [][]string{{"bad\xff"}}}
	got := Format(questions, reply)
	want := "User answers:\n- H (Q): \"bad\uFFFD\""
	if string(got.Output) != want || !got.OK {
		t.Fatalf("Format = %q ok=%v, want %q ok=true", got.Output, got.OK, want)
	}
}

func TestFormatOneAnswerForSingleSelect(t *testing.T) {
	questions := []Question{{Header: "Database", Text: "Which database should the service use?", Options: []Option{{Label: "SQLite"}, {Label: "Postgres"}}}}
	reply := Reply{Outcome: OutcomeAnswered, Answers: [][]string{{"Postgres", "SQLite"}}}
	got := Format(questions, reply)
	want := "User answers:\n- Database (Which database should the service use?): SQLite"
	if string(got.Output) != want {
		t.Fatalf("Format = %q, want %q", got.Output, want)
	}
}

func TestFormatCapsTypedAnswerAtMaxAnswer(t *testing.T) {
	questions := []Question{{Header: "Name", Text: "Name?"}}
	reply := Reply{Outcome: OutcomeAnswered, Answers: [][]string{{repeatRune('a', 2500)}}}
	got := Format(questions, reply)
	want := "User answers:\n- Name (Name?): \"" + repeatRune('a', MaxAnswer) + "\""
	if string(got.Output) != want {
		t.Fatalf("Format output is %d bytes, want %d bytes capped at %d runes", len(got.Output), len(want), MaxAnswer)
	}
}

func TestFormatUndefinedOutcomeIsExplicitFailure(t *testing.T) {
	got := Format([]Question{{Header: "H", Text: "Q"}}, Reply{})
	if string(got.Output) != TextUnknownOutcome || got.OK {
		t.Fatalf("Format = %q ok=%v, want %q ok=false", got.Output, got.OK, TextUnknownOutcome)
	}
}

func TestTextConstants(t *testing.T) {
	if TextNoClient != "declined: no client could answer this question" {
		t.Fatalf("TextNoClient = %q", TextNoClient)
	}
	if TextRestarted != "interrupted: the host restarted before the user answered" {
		t.Fatalf("TextRestarted = %q", TextRestarted)
	}
	if QuestionID(0) != "q1" || QuestionID(3) != "q4" {
		t.Fatalf("QuestionID rule broken: %q %q", QuestionID(0), QuestionID(3))
	}
}
