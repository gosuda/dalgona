package tui

import (
	"strings"
	"testing"

	"charm.land/bubbles/v2/spinner"
	tea "charm.land/bubbletea/v2"
)

func reviewThreadStarted() notificationMsg {
	return notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)}
}

func reviewModel(t *testing.T, width, lines int) model {
	t.Helper()
	m := newTestModel(new(fakeCaller), Options{})
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: width, Height: 24})
	m = rawUpdate(t, m, reviewThreadStarted())
	m = rawUpdate(t, m, commandOutput("seq", numberedOutput(lines)))
	return settleFrames(m)
}

func TestReviewShowsFullTextAndCloses(t *testing.T) {
	t.Parallel()
	m := reviewModel(t, 80, 300)
	m = rawUpdate(t, m, press('o', tea.ModCtrl, ""))
	if m.review == nil {
		t.Fatal("ctrl+o did not open the review")
	}
	m = rawUpdate(t, m, press('G', 0, "G"))
	view := m.View()
	if !view.AltScreen {
		t.Fatal("the open review does not use the alternate screen")
	}
	if !strings.Contains(view.Content, "out-300") {
		t.Fatalf("line 300 is not reachable with G:\n%s", view.Content)
	}
	m = rawUpdate(t, m, press(tea.KeyEscape, 0, ""))
	if m.View().AltScreen {
		t.Fatal("esc did not close the review")
	}
}

func TestReviewFrameBounded(t *testing.T) {
	t.Parallel()
	for _, width := range []int{40, 100} {
		m := reviewModel(t, width, 300)
		m = rawUpdate(t, m, press('o', tea.ModCtrl, ""))
		assertFrame(t, strings.Split(m.View().Content, "\n"), width, 24)
	}
}

func TestReviewHoldsRowsUntilClose(t *testing.T) {
	t.Parallel()
	m := newTestModel(new(fakeCaller), Options{})
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m = rawUpdate(t, m, reviewThreadStarted())
	m = rawUpdate(t, m, settledMsg{n: m.out.settleN})
	m = rawUpdate(t, m, commandOutput("one", "one"))
	m = rawUpdate(t, m, printedMsg{})
	m = rawUpdate(t, m, press('o', tea.ModCtrl, ""))
	held := len(m.out.pending)
	m = rawUpdate(t, m, settledMsg{n: m.out.settleN})
	m = rawUpdate(t, m, commandOutput("two", "two"))
	if len(m.out.pending) <= held || m.out.printing {
		t.Fatalf("a row finished while the review is open: pending %d, printing %t", len(m.out.pending), m.out.printing)
	}
	m = rawUpdate(t, m, press(tea.KeyEscape, 0, ""))
	if !m.out.settling || m.out.printing {
		t.Fatalf("closing the review must wait for the frame to settle: settling %t, printing %t", m.out.settling, m.out.printing)
	}
	m = rawUpdate(t, m, settledMsg{n: m.out.settleN})
	if !m.out.printing || len(m.out.pending) != 0 {
		t.Fatalf("the held row did not print after close: printing %t, pending %d", m.out.printing, len(m.out.pending))
	}
}

func TestReviewOpensAndClosesOnlyByKey(t *testing.T) {
	t.Parallel()
	m := reviewModel(t, 80, 3)
	m = rawUpdate(t, m, press('o', tea.ModCtrl, ""))
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 80, Height: 30})
	m = rawUpdate(t, m, spinner.TickMsg{})
	if !m.View().AltScreen {
		t.Fatal("a size or tick message closed the review")
	}
	m = rawUpdate(t, m, press(tea.KeyEscape, 0, ""))
	if m.View().AltScreen {
		t.Fatal("esc did not close the review")
	}
}

func TestReviewOfApprovalShowsCommandOnlyWhenPresent(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name, method, params, want, absent string
	}{
		{
			name:   "command approval",
			method: "item/commandExecution/requestApproval",
			params: `{"threadId":"t1","turnId":"turn-1","itemId":"cmd1","startedAtMs":1,"command":"make all"}`,
			want:   "$ make all",
		},
		{
			name:   "file change approval",
			method: "item/fileChange/requestApproval",
			params: `{"threadId":"t1","turnId":"turn-1","itemId":"fc1","startedAtMs":1,"reason":"edit a.go"}`,
			want:   "edit a.go",
			absent: "$ ",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := reviewModel(t, 80, 3)
			m = rawUpdate(t, m, serverRequestMsg{method: tc.method, answer: make(chan requestAnswer, 1), params: []byte(tc.params)})
			m = rawUpdate(t, m, press('o', tea.ModCtrl, ""))
			if m.review == nil || m.review.live == nil {
				t.Fatal("ctrl+o did not open the review on the open approval")
			}
			text := m.review.live.text
			if !strings.Contains(text, tc.want) {
				t.Fatalf("review of the approval lacks %q:\n%s", tc.want, text)
			}
			if tc.absent != "" && strings.Contains(text, tc.absent) {
				t.Fatalf("review of the approval has a bare command line %q:\n%s", tc.absent, text)
			}
		})
	}
}

func TestReviewRequestsFocusReports(t *testing.T) {
	t.Parallel()
	m := reviewModel(t, 80, 3)
	m = rawUpdate(t, m, press('o', tea.ModCtrl, ""))
	if !m.View().ReportFocus {
		t.Fatal("the open review does not request focus reports")
	}
}

func TestReviewHeaderCountsHeldRows(t *testing.T) {
	t.Parallel()
	m := reviewModel(t, 80, 3)
	m = rawUpdate(t, m, press('o', tea.ModCtrl, ""))
	m.commit("held:1", []string{"one"})
	m.commit("held:2", []string{"two"})
	header, _, _ := strings.Cut(m.reviewView(), "\n")
	if !strings.Contains(header, "· 2 waiting") {
		t.Fatalf("the review header does not count the two held rows: %q", header)
	}
}

func judgedToolNotification(verdict, summary string) notificationMsg {
	return notificationMsg{method: "dalgona/tool/judged", params: []byte(
		`{"threadId":"t1","turnId":"turn-1","itemId":"call_7","verdict":"` + verdict + `","summary":"` + summary + `","paths":["a.go"]}`)}
}

func judgedTurnNotification(verdict, summary string) notificationMsg {
	return notificationMsg{method: "dalgona/turn/judged", params: []byte(
		`{"threadId":"t1","turnId":"turn-1","verdict":"` + verdict + `","summary":"` + summary + `","paths":["a.go"]}`)}
}

func TestJudgedToolRowKeyIsItemScoped(t *testing.T) {
	t.Parallel()
	m := schemaStartedModel(t)
	m, _ = drive(t, m, judgedToolNotification("flagged", "Flagged: removed code"))
	if len(m.out.entries) == 0 {
		t.Fatal("flagged tool review printed no row")
	}
	if key := m.out.entries[len(m.out.entries)-1].key; key != "review:t1:call_7" {
		t.Fatalf("tool review row key %q, want review:t1:call_7", key)
	}
	before := len(m.out.entries)
	m, _ = drive(t, m, judgedToolNotification("flagged", "Flagged: removed code"))
	if len(m.out.entries) != before {
		t.Fatalf("a repeated tool review added a row: %d entries, want %d", len(m.out.entries), before)
	}
}

func TestToolJudgedPrintsFlaggedRow(t *testing.T) {
	t.Parallel()
	m := schemaStartedModel(t)
	m, command := drive(t, m, judgedToolNotification("flagged", "Flagged: removed code"))
	if command == nil {
		t.Fatal("flagged review did not print a row")
	}
	assertContains(t, lastEntry(t, m), "review", "Flagged: removed code", "a.go")
	m.screen.width = 120
	row := m.renderReviewRow("flagged", "Flagged: removed code", []string{"a.go"})
	text := strings.Join(row, "\n")
	first := strings.Index(text, "review")
	second := strings.Index(text, "Flagged: removed code")
	third := strings.Index(text, "a.go")
	if first < 0 || second < first || third < second {
		t.Fatalf("row %q does not read review, summary, path in order", text)
	}
}

func TestToolJudgedCleanPrintsNothing(t *testing.T) {
	t.Parallel()
	m := schemaStartedModel(t)
	before := len(m.out.entries)
	m, command := drive(t, m, judgedToolNotification("clean", "No problems found"))
	if command != nil || len(m.out.pending) != 0 || len(m.out.entries) != before {
		t.Fatalf("clean review printed: command %v, pending %q", command, m.out.pending)
	}
}

func TestReviewRowVerbosity(t *testing.T) {
	t.Parallel()
	cases := []struct {
		verdict, summary string
		level            string
		want             bool
	}{
		{"flagged", "Flagged: removed code", "low", true},
		{"flagged", "Flagged: removed code", "medium", true},
		{"flagged", "Flagged: removed code", "high", true},
		{"error", "Failed: boom", "low", true},
		{"error", "Failed: boom", "medium", true},
		{"error", "Failed: boom", "high", true},
		{"uncertain", "Uncertain: removed code", "low", false},
		{"uncertain", "Uncertain: removed code", "medium", false},
		{"uncertain", "Uncertain: removed code", "high", true},
		{"clean", "No problems found", "low", false},
		{"clean", "No problems found", "medium", false},
		{"clean", "No problems found", "high", false},
	}
	for _, tc := range cases {
		for _, method := range []string{"tool", "turn"} {
			m := schemaStartedModel(t)
			m.screen.verbosity = tc.level
			before := len(m.out.entries)
			note := judgedToolNotification(tc.verdict, tc.summary)
			if method == "turn" {
				note = judgedTurnNotification(tc.verdict, tc.summary)
			}
			m, _ = drive(t, m, note)
			if got := len(m.out.entries) > before; got != tc.want {
				t.Fatalf("%s %s at %s: printed %t, want %t", method, tc.verdict, tc.level, got, tc.want)
			}
		}
	}
}

func TestReviewRowErrorIsBold(t *testing.T) {
	t.Parallel()
	m := newTestModel(new(fakeCaller), Options{})
	m.screen.width = 120
	m.screen.theme = newTheme(themeNight, true)
	failed := strings.Join(m.renderReviewRow("error", "Failed: boom", []string{"a.go"}), "\n")
	if !strings.Contains(failed, "\x1b[1;") {
		t.Fatalf("error row is not bold: %q", failed)
	}
	flagged := strings.Join(m.renderReviewRow("flagged", "Flagged: removed code", []string{"a.go"}), "\n")
	if strings.Contains(flagged, "\x1b[1;") || strings.Contains(flagged, "\x1b[1m") {
		t.Fatalf("flagged row is bold: %q", flagged)
	}
}
