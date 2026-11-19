package tui

import (
	"errors"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestAppendSanitizedDropsBidiAndPrivateUse(t *testing.T) {
	t.Parallel()
	got := appendSanitized([]byte("x"), []byte("a\u061cb\u200ec\u202ad\u2069e\U000f0000f\x1b[1mg"))
	if string(got) != "xabcdefg" {
		t.Fatalf("appendSanitized = %q, want %q", got, "xabcdefg")
	}
}

func committedText(m model) string {
	var b strings.Builder
	for _, entry := range m.out.entries {
		b.WriteString(entry.text)
		b.WriteByte('\n')
	}
	return b.String()
}

type untrustedCase struct {
	name    string
	msg     tea.Msg
	prepare func(t *testing.T) (model, tea.Msg)
}

func untrustedSettingsSave(t *testing.T) (model, tea.Msg) {
	t.Helper()
	m := openSettingsPanel(t, panelPicker(t, new(fakeCaller)), "ask")
	m = pressKeys(t, m, tea.KeyPressMsg{Code: tea.KeyRight})
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	return m, resultMsg{op: "settingsSave", err: errors.New(hostileAPC)}
}

func untrustedAdvisorList(err error) func(t *testing.T) (model, tea.Msg) {
	return func(t *testing.T) (model, tea.Msg) {
		t.Helper()
		m := advisorPanelOpen(t, panelPicker(t, new(fakeCaller)), []rpc.AdvisorState{{Name: "advisor", Status: "idle"}})
		return m, resultMsg{op: "advisorSet", err: err}
	}
}

func untrustedAdvisorNotice(t *testing.T) (model, tea.Msg) {
	t.Helper()
	return panelPicker(t, new(fakeCaller)), resultMsg{op: "advisorStatus", err: errors.New(hostileAPC)}
}

func untrustedFileSearch(t *testing.T) (model, tea.Msg) {
	t.Helper()
	m := newTestModel(&fileSearchCaller{}, Options{Cwd: "/work"})
	m, _ = typeCommand(t, m, "@a")
	return m, searchResultMsg{seq: m.input.popup.seq, files: []fileHit{{Path: hostileAPC}}}
}

const hostileAPC = "x\u001b_Gpwn\u001b\\y\u202e"

func TestUntrustedTextIsSanitizedAtEveryEntry(t *testing.T) {
	t.Parallel()
	const hostile = "x\u001b]0;pwn\u0007y\u202e"
	cases := []untrustedCase{
		{name: "agentMessage delta", msg: notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":"x\u001b]0;pwn\u0007y\u202e"}`)}},
		{name: "commandExecution output delta", msg: notificationMsg{method: "item/commandExecution/outputDelta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"c1","delta":"x\u001b]0;pwn\u0007y\u202e"}`)}},
		{name: "item/started command", msg: notificationMsg{method: "item/started", params: []byte(`{"threadId":"t1","turnId":"turn-1","item":{"type":"commandExecution","id":"c1","command":"x\u001b]0;pwn\u0007y\u202e"}}`)}},
		{name: "error message", msg: notificationMsg{method: "error", params: []byte(`{"threadId":"t1","turnId":"turn-1","willRetry":false,"error":{"message":"x\u001b]0;pwn\u0007y\u202e"}}`)}},
		{name: "plan step", msg: notificationMsg{method: "turn/plan/updated", params: []byte(`{"threadId":"t1","turnId":"turn-1","plan":[{"step":"x\u001b]0;pwn\u0007y\u202e","status":"inProgress"}]}`)}},
		{name: "advisor note", msg: notificationMsg{method: "dalgona/advisor/note", params: []byte(`{"threadId":"t1","advisor":"x\u001b]0;pwn\u0007y\u202e","severity":"info","note":"x\u001b]0;pwn\u0007y\u202e"}`)}},
		{name: "model display name", msg: resultMsg{op: "models", value: &rpc.ModelListResponse{Data: []rpc.Model{{ID: "m1", DisplayName: hostile}}}}},
		{name: "thread list title", msg: resultMsg{op: "threads", value: &rpc.ThreadListResponse{Data: []rpc.Thread{{ID: "t2", Name: hostile}}}}},
		{name: "paste", msg: tea.PasteMsg{Content: hostile}},
		{name: "login url", msg: resultMsg{op: "login", value: &rpc.LoginAccountResponse{AuthURL: "https://x\u001b]0;pwn\u0007y\u202e"}}},
		{name: "settings save error", prepare: untrustedSettingsSave},
		{name: "advisor failure in the open list", prepare: untrustedAdvisorList(errors.New(hostileAPC))},
		{name: "advisor unavailable in the open list", prepare: untrustedAdvisorList(&rpc.RPCError{Code: -32001, Message: hostileAPC})},
		{name: "advisor failure notice", prepare: untrustedAdvisorNotice},
		{name: "shell output", msg: shellResultMsg{n: 1, command: "echo " + hostileAPC, result: rpc.CommandExecResponse{Stdout: hostileAPC, Stderr: hostileAPC}}},
		{name: "shell error", msg: shellResultMsg{n: 1, command: "echo " + hostileAPC, err: errors.New(hostileAPC)}},
		{name: "file search row", prepare: untrustedFileSearch},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := newModel(nil, Options{}, nil)
			m.thread.id = "t1"
			msg := tc.msg
			if tc.prepare != nil {
				m, msg = tc.prepare(t)
				m.thread.id = "t1"
			}
			m, _ = drive(t, m, msg)
			shown := m.View().Content + "\n" + committedText(m) + "\n" + m.liveText()
			if !strings.Contains(shown, "xy") {
				t.Errorf("view lost the visible text:\n%q", shown)
			}
			for _, forbidden := range []string{"\x1b]0;", "\x1b_", "pwn", "\u202e"} {
				if strings.Contains(shown, forbidden) {
					t.Errorf("view kept untrusted control text %q:\n%q", forbidden, shown)
				}
			}
		})
	}
}

func TestUntrustedThreadNameIsSanitizedInState(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.thread.id = "t1"
	m, _ = drive(t, m, notificationMsg{method: "thread/name/updated", params: []byte(`{"threadId":"t1","threadName":"x\u001b]0;pwn\u0007y\u202e"}`)})
	if m.thread.title != "xy" {
		t.Fatalf("thread title = %q, want the visible text xy", m.thread.title)
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
