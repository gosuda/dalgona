package tui

import (
	json "encoding/json/v2"
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
	m := newTestModel(nil, Options{}, nil)
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
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, commandApprovalRequest(make(chan requestAnswer, 1)))
	m, _ = drive(t, m, press('o', tea.ModCtrl, ""))
	assertContains(t, m.View().Content, "Review · ")
}

func TestChildFileApprovalShowsDiff(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
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
	m := newTestModel(nil, Options{}, nil)
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
	m := newTestModel(nil, Options{}, nil)
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
	m := newTestModel(nil, Options{}, nil)
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
	m := newTestModel(nil, Options{}, nil)
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
