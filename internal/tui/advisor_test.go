package tui

import (
	"errors"
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

const advisorItem = `{"type":"dynamicToolCall","id":"adv1","tool":"advisor","arguments":{"advisor":"advisor","severity":"concern","delivery":"aside"},` +
	`"status":"completed","contentItems":[{"type":"inputText","text":"run the race detector before merging"}],"success":true,"durationMs":1}`

func TestModelPrintsAdvisorNoteOnce(t *testing.T) {
	t.Parallel()
	m := schemaStartedModel(t)
	m, command := drive(t, m, notificationMsg{method: "dalgona/advisor/note", params: []byte(
		`{"threadId":"t1","advisor":"advisor","severity":"concern","note":"run the race detector before merging","delivery":"aside"}`)})
	if command == nil {
		t.Fatal("advisor note did not print a card")
	}
	assertContains(t, lastEntry(t, m), "advisor · concern: run the race detector before merging")
	m = schemaNotification(t, m, "item/started", "ItemStartedNotification",
		`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":`+strings.Replace(advisorItem, `"completed"`, `"inProgress"`, 1)+`}`)
	if strings.Contains(m.View().Content, "advisor") {
		t.Fatalf("live advisor item rendered in the live view:\n%s", m.View().Content)
	}
	m, command = drive(t, m, notificationMsg{method: "item/completed", params: schemaValue(t, "ItemCompletedNotification",
		`{"threadId":"t1","turnId":"turn-1","completedAtMs":2,"item":`+advisorItem+`}`)})
	if command != nil || len(m.live.items) != 0 {
		t.Fatalf("completed advisor item printed again: command %v, live %#v", command, m.live.items)
	}
}

func TestModelRendersResumedAdvisorItemAsCard(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 120
	note := "run the race detector before merging"
	item := rpc.Item{
		Type: "dynamicToolCall", ID: "adv1", Tool: "advisor", Status: "completed", Output: note,
		Arguments: []byte(`{"advisor":"watch","severity":"blocker","delivery":"steer"}`),
	}
	assertContains(t, stripANSI(settledText(&m, item)), "advisor watch · blocker: "+note)
	item.Arguments = []byte(`[]`)
	assertContains(t, stripANSI(settledText(&m, item)), "Advisor", "Done", note)
}

func TestModelAdvisorCommands(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.input.composer.SetValue("/advisor")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	m, command = drive(t, m, command())
	if command == nil || caller.calls[0].method != "dalgona/advisor/status" {
		t.Fatalf("toggle did not follow the status read with a set call; calls %#v", caller.calls)
	}
	m, _ = drive(t, m, command())
	set, ok := caller.calls[1].params.(rpc.DalgonaAdvisorSetParams)
	if caller.calls[1].method != "dalgona/advisor/set" || !ok || set.Enabled || set.ThreadID != "t1" {
		t.Fatalf("toggle set call = %#v", caller.calls[1])
	}
	assertContains(t, m.notice, "advisor off", "/advisor")
	m.input.composer.SetValue("/advisor status")
	m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
	m = runCommand(t, m, command, "/advisor status made no call")
	assertContains(t, m.notice, "advisor advisor · idle · gpt-test · 2 notes")
	unavailable := fmt.Errorf("rpc error -32001: %w", &rpc.RPCError{Code: -32001, Message: "advisor runtime is not available"})
	m, _ = drive(t, m, resultMsg{op: "advisorReset", err: unavailable})
	assertContains(t, m.notice, "advisor unavailable: advisor runtime is not available", "config.toml")
	m, _ = drive(t, m, resultMsg{op: "advisorStatus", err: errors.New("connection closed")})
	assertContains(t, m.notice, "advisor request failed: connection closed", "retry")
}

func TestModelQueuesApprovalsAndIgnoresOtherThreads(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 40
	m.thread.id = "t1"
	first, second := make(chan requestAnswer, 1), make(chan requestAnswer, 1)
	m, _ = drive(t, m, serverRequestMsg{
		method: "item/commandExecution/requestApproval", answer: first,
		params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"c1","startedAtMs":1,"command":"go test ./...","reason":"verify the fix"}`),
	})
	m, _ = drive(t, m, serverRequestMsg{
		method: "item/fileChange/requestApproval", answer: second,
		params: []byte(`{"threadId":"child-1","turnId":"turn-2","itemId":"f1","startedAtMs":2}`),
	})
	view := m.View().Content
	assertContains(t, view, "approval required (1 of 2)", "reason: verify the fix", "[esc] cancel and interrupt")
	for line := range strings.SplitSeq(view, "\n") {
		if lipgloss.Width(line) > 40 {
			t.Errorf("approval line wider than 40: %q", line)
		}
	}
	m, _ = drive(t, m, press('y', 0, "y"))
	if answer := <-first; answer.value != (rpc.CommandExecutionRequestApprovalResponse{Decision: "accept"}) {
		t.Fatalf("first answer = %#v", answer)
	}
	assertContains(t, m.View().Content, "approval required", "thread child-1")
	m, _ = drive(t, m, press('n', 0, "n"))
	if answer := <-second; answer.value != (rpc.CommandExecutionRequestApprovalResponse{Decision: "decline"}) {
		t.Fatalf("second answer = %#v", answer)
	}
	m, _ = drive(t, m, notificationMsg{method: "thread/status/changed", params: []byte(`{"threadId":"child-1","status":{"type":"systemError"}}`)})
	m, _ = drive(t, m, notificationMsg{method: "thread/name/updated", params: []byte(`{"threadId":"child-1","threadName":"child"}`)})
	if m.thread.id != "t1" || m.thread.state != stateRunning || m.thread.title == "child" || len(m.approvals) != 0 {
		t.Fatalf("child notifications changed thread %q state %q title %q approvals %d", m.thread.id, m.thread.state, m.thread.title, len(m.approvals))
	}
}

func TestComposerPlaceholderFollowsCursor(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	assertContains(t, stripANSI(m.View().Content), "› Send a prompt · / commands · ? keys")
	m.input.composer.SetValue("typed")
	view := stripANSI(m.View().Content)
	if strings.Contains(view, "Send a prompt") {
		t.Fatalf("placeholder shown beside typed text:\n%s", view)
	}
}
