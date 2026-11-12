package tui

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func TestAppendSanitizedDropsBidiAndPrivateUse(t *testing.T) {
	t.Parallel()
	got := appendSanitized([]byte("x"), []byte("a\u061cb\u200ec\u202ad\u2069e\U000f0000f\x1b[1mg"))
	if string(got) != "xabcdefg" {
		t.Fatalf("appendSanitized = %q, want %q", got, "xabcdefg")
	}
}

func printedText(command tea.Cmd) string {
	if command == nil {
		return ""
	}
	return fmt.Sprint(command())
}

func TestUntrustedTextIsSanitizedAtEveryEntry(t *testing.T) {
	t.Parallel()
	const hostile = "x\u001b]0;pwn\u0007y\u202e"
	cases := []struct {
		name string
		msg  tea.Msg
	}{
		{"agentMessage delta", notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":"x\u001b]0;pwn\u0007y\u202e"}`)}},
		{"commandExecution output delta", notificationMsg{method: "item/commandExecution/outputDelta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"c1","delta":"x\u001b]0;pwn\u0007y\u202e"}`)}},
		{"item/started command", notificationMsg{method: "item/started", params: []byte(`{"threadId":"t1","turnId":"turn-1","item":{"type":"commandExecution","id":"c1","command":"x\u001b]0;pwn\u0007y\u202e"}}`)}},
		{"error message", notificationMsg{method: "error", params: []byte(`{"threadId":"t1","turnId":"turn-1","willRetry":false,"error":{"message":"x\u001b]0;pwn\u0007y\u202e"}}`)}},
		{"thread name", notificationMsg{method: "thread/name/updated", params: []byte(`{"threadId":"t1","threadName":"x\u001b]0;pwn\u0007y\u202e"}`)}},
		{"plan step", notificationMsg{method: "turn/plan/updated", params: []byte(`{"threadId":"t1","turnId":"turn-1","plan":[{"step":"x\u001b]0;pwn\u0007y\u202e","status":"inProgress"}]}`)}},
		{"advisor note", notificationMsg{method: "dalgona/advisor/note", params: []byte(`{"threadId":"t1","advisor":"x\u001b]0;pwn\u0007y\u202e","severity":"info","note":"x\u001b]0;pwn\u0007y\u202e"}`)}},
		{"model display name", resultMsg{op: "models", value: &rpc.ModelListResponse{Data: []rpc.Model{{ID: "m1", DisplayName: hostile}}}}},
		{"thread list title", resultMsg{op: "threads", value: &rpc.ThreadListResponse{Data: []rpc.Thread{{ID: "t2", Name: hostile}}}}},
		{"paste", tea.PasteMsg{Content: hostile}},
		{"login url", resultMsg{op: "login", value: &rpc.LoginAccountResponse{AuthURL: "https://x\u001b]0;pwn\u0007y\u202e"}}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := newModel(nil, Options{}, nil)
			m.thread.id = "t1"
			m, command := drive(t, m, tc.msg)
			shown := m.View().Content + "\n" + printedText(command)
			if !strings.Contains(shown, "xy") {
				t.Errorf("view lost the visible text:\n%q", shown)
			}
			if strings.Contains(shown, "\x1b]0;") || strings.Contains(shown, "\u202e") {
				t.Errorf("view kept untrusted control text:\n%q", shown)
			}
		})
	}
}

func TestUntrustedApprovalThreadIsSanitized(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.thread.id = "t1"
	answer := make(chan requestAnswer, 1)
	params := []byte(`{"threadId":"t1\u001b]0;pwn\u0007y\u202e","turnId":"turn-1","itemId":"c1","command":"echo hi","reason":"why"}`)
	m, _ = drive(t, m, serverRequestMsg{method: "item/commandExecution/requestApproval", params: params, answer: answer})
	view := m.View().Content
	if !strings.Contains(view, "thread t1y\n") {
		t.Fatalf("approval view lost the thread id text:\n%q", view)
	}
	if strings.Contains(view, "\x1b]0;") || strings.Contains(view, "\u202e") {
		t.Fatalf("approval view kept untrusted control text:\n%q", view)
	}
}
