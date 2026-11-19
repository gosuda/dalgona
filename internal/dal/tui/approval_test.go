package tui

import (
	json "encoding/json/v2"
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func fileApprovalRequest(answer chan requestAnswer) serverRequestMsg {
	return serverRequestMsg{
		method: "item/fileChange/requestApproval",
		params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"fc1","startedAtMs":1,"reason":"edit a.go"}`),
		answer: answer,
	}
}

func commandApprovalRequest(answer chan requestAnswer) serverRequestMsg {
	return serverRequestMsg{
		method: "item/commandExecution/requestApproval",
		params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"cmd1","startedAtMs":1,"command":"go test ./..."}`),
		answer: answer,
	}
}

func TestFileApprovalShowsPathsAndDiff(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, notificationMsg{
		method: "item/fileChange/patchUpdated",
		params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"fc1","changes":[{"path":"a.go","kind":{"type":"update"},"diff":"--- a/a.go\n+++ b/a.go\n@@ -1 +1 @@\n-old\n+new\n","additions":1,"deletions":1}]}`),
	})
	m, _ = drive(t, m, fileApprovalRequest(make(chan requestAnswer, 1)))
	assertContains(t, m.View().Content, "Apply file changes", "Edited a.go +1 −1", "-old", "+new")
}

func TestApprovalCtrlOOpensReview(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, commandApprovalRequest(make(chan requestAnswer, 1)))
	m, _ = drive(t, m, press('o', tea.ModCtrl, ""))
	assertContains(t, m.View().Content, "Review · ")
}

func TestChildFileApprovalShowsDiff(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
	m, _ = drive(t, m, notificationMsg{
		method: "item/fileChange/patchUpdated",
		params: []byte(`{"threadId":"child","turnId":"turn-2","itemId":"fc1","changes":[{"path":"a.go","kind":{"type":"update"},"diff":"--- a/a.go\n+++ b/a.go\n@@ -1 +1 @@\n-old\n+new\n","additions":1,"deletions":1}]}`),
	})
	m, _ = drive(t, m, serverRequestMsg{
		method: "item/fileChange/requestApproval",
		params: []byte(`{"threadId":"child","turnId":"turn-2","itemId":"fc1","startedAtMs":1,"reason":"edit a.go"}`),
		answer: make(chan requestAnswer, 1),
	})
	assertContains(t, m.View().Content, "From thread child", "Edited a.go +1 −1", "-old", "+new")
}

func TestApprovalNDeclinesAtOnce(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	answer := make(chan requestAnswer, 1)
	m, _ = drive(t, m, commandApprovalRequest(answer))
	m, _ = drive(t, m, press('n', 0, "n"))
	response := <-answer
	if response.err != nil {
		t.Fatalf("response error: %v", response.err)
	}
	raw, err := json.Marshal(response.value)
	if err != nil {
		t.Fatal(err)
	}
	var members map[string]any
	if err := json.Unmarshal(raw, &members); err != nil {
		t.Fatal(err)
	}
	if len(members) != 1 || members["decision"] != "decline" {
		t.Fatalf("response = %s; want exactly {\"decision\":\"decline\"}", raw)
	}
	if len(m.approvals) != 0 {
		t.Fatalf("card still open after n: %d approvals", len(m.approvals))
	}
}

func TestApprovalNOpensNoTextMode(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, commandApprovalRequest(make(chan requestAnswer, 1)))
	m, _ = drive(t, m, press('n', 0, "n"))
	if m.list != nil || m.confirm != nil {
		t.Fatal("n opened a panel")
	}
	if strings.Contains(m.View().Content, "answer:") || strings.Contains(m.View().Content, "filter:") {
		t.Fatal("n opened a text mode")
	}
}

func TestApprovalCardSteady(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	long, err := json.Marshal(strings.Repeat("go test ./... -count=1\n", 60))
	if err != nil {
		t.Fatal(err)
	}
	m, _ = drive(t, m, serverRequestMsg{
		method: "item/commandExecution/requestApproval",
		answer: make(chan requestAnswer, 1),
		params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"cmd1","startedAtMs":1,"command":` + string(long) + `}`),
	})
	keys := make([]tea.KeyPressMsg, 50)
	for i := range keys {
		keys[i] = tea.KeyPressMsg{Code: tea.KeyDown}
	}
	assertSteadyDock(t, m, keys)
}

func TestApprovalResolvedByAnotherWindowCloses(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, serverRequestMsg{
		method:    "item/commandExecution/requestApproval",
		requestID: "req-1",
		params:    []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"cmd1","startedAtMs":1,"command":"go test ./..."}`),
		answer:    make(chan requestAnswer, 1),
	})
	m, _ = drive(t, m, notificationMsg{
		method: "serverRequest/resolved",
		params: []byte(`{"threadId":"t1","requestId":"req-1","resolution":"answered"}`),
	})
	if len(m.approvals) != 0 {
		t.Fatal("approval still open after another window answered it")
	}
	const notice = "Approval answered in another window"
	var notices int
	for _, entry := range m.out.entries {
		if stripANSI(entry.text) == notice {
			notices++
		}
	}
	if notices != 1 {
		t.Fatalf("full resolution notice committed %d times, want once: %+v", notices, m.out.entries)
	}
}

func tenLineApproval() serverRequestMsg {
	var command []string
	for i := range 10 {
		command = append(command, fmt.Sprintf("row%d", i))
	}
	encoded, err := json.Marshal(strings.Join(command, "\n"))
	if err != nil {
		panic(err)
	}
	return serverRequestMsg{
		method: "item/commandExecution/requestApproval",
		answer: make(chan requestAnswer, 1),
		params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"cmd1","startedAtMs":1,"command":` + string(encoded) + `}`),
	}
}

func approvalBodyRows(m model) []string {
	var rows []string
	for _, line := range strings.Split(stripANSI(m.View().Content), "\n") {
		if body, ok := strings.CutPrefix(line, "│   "); ok {
			rows = append(rows, strings.TrimRight(body, " "))
		}
	}
	return rows
}

func pressTimes(t *testing.T, m model, code rune, times int) model {
	t.Helper()
	for range times {
		m, _ = drive(t, m, tea.KeyPressMsg{Code: code})
	}
	return m
}

func TestApprovalScrollClampsAtEnd(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		height   int
		atEnd    string
		afterUp  string
		bodyRows int
	}{
		{height: 24, atEnd: "row4", afterUp: "row3", bodyRows: 6},
		{height: 12, atEnd: "row5", afterUp: "row4", bodyRows: 5},
	} {
		t.Run(fmt.Sprintf("height%d", tc.height), func(t *testing.T) {
			t.Parallel()
			m := newTestModel(nil, Options{})
			m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: tc.height})
			m, _ = drive(t, m, tenLineApproval())
			m = pressTimes(t, m, tea.KeyDown, 50)
			rows := approvalBodyRows(m)
			if len(rows) != tc.bodyRows || rows[0] != tc.atEnd || rows[len(rows)-1] != "row9" {
				t.Fatalf("after 50 downs the body rows are %q, want %d rows from %s to row9", rows, tc.bodyRows, tc.atEnd)
			}
			m = pressTimes(t, m, tea.KeyUp, 1)
			rows = approvalBodyRows(m)
			if len(rows) != tc.bodyRows || rows[0] != tc.afterUp || !strings.HasPrefix(rows[len(rows)-1], "↓ 2 more lines") {
				t.Fatalf("after one up the body rows are %q, want %d rows from %s ending with the 2 more lines row", rows, tc.bodyRows, tc.afterUp)
			}
			m = pressTimes(t, m, tea.KeyDown, 1)
			if rows = approvalBodyRows(m); rows[0] != tc.atEnd {
				t.Fatalf("down after up shows %q first, want %s", rows[0], tc.atEnd)
			}
		})
	}
}

func TestApprovalScrollClampsAfterResize(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 12})
	m, _ = drive(t, m, tenLineApproval())
	m = pressTimes(t, m, tea.KeyDown, 50)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	if rows := approvalBodyRows(m); rows[0] != "row4" {
		t.Fatalf("after the window grows the body starts at %q, want row4", rows[0])
	}
	m = pressTimes(t, m, tea.KeyUp, 1)
	if rows := approvalBodyRows(m); rows[0] != "row3" {
		t.Fatalf("after one up the body starts at %q, want row3", rows[0])
	}
}
