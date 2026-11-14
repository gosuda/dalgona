package tui

import (
	"reflect"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func flatten(command tea.Cmd) []tea.Msg {
	if command == nil {
		return nil
	}
	msg := command()
	batch, ok := msg.(tea.BatchMsg)
	if !ok {
		return []tea.Msg{msg}
	}
	var out []tea.Msg
	for _, next := range batch {
		out = append(out, flatten(next)...)
	}
	return out
}

func TestWindowTitleShowsState(t *testing.T) {
	t.Parallel()
	m := playTerminal(newModel(nil, Options{}, nil))
	m.conn.product = "dalgona"
	m.thread.title = "Plan"
	m.thread.state = stateRunning
	if got := m.View().WindowTitle; got != "dalgona · Plan · Working" {
		t.Fatalf("window title = %q, want %q", got, "dalgona · Plan · Working")
	}
}

func TestViewRequestsFocusReports(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	if !m.View().ReportFocus {
		t.Fatal("view before the first row does not request focus reports")
	}
	m.leaving = true
	if !m.View().ReportFocus {
		t.Fatal("leaving view does not request focus reports")
	}
}

func TestNotifyOnlyWhenBlurred(t *testing.T) {
	t.Parallel()
	m := playTerminal(newModel(nil, Options{}, nil))
	m.screen.notifications = true
	m, _ = drive(t, m, tea.BlurMsg{})
	if got := flatten(m.notify("Done")); !reflect.DeepEqual(got, []tea.Msg{tea.RawMsg{Msg: "\a"}}) {
		t.Fatalf("blurred notify = %#v, want the bell", got)
	}
	m, _ = drive(t, m, tea.FocusMsg{})
	if m.notify("Done") != nil {
		t.Fatal("focused window sent a desktop notification")
	}
}

func TestNotifyOffSendsNothing(t *testing.T) {
	t.Parallel()
	m := playTerminal(newModel(nil, Options{}, nil))
	m.screen.notifications = false
	m, _ = drive(t, m, tea.BlurMsg{})
	if m.notify("Done") != nil {
		t.Fatal("notifications off still sent a desktop notification")
	}
}

func TestNotifySequenceFollowsTerminal(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		env  map[string]string
		want string
	}{
		{"tmux bell", map[string]string{"TMUX": "/tmp/tmux,1,0", "TERM": "xterm-kitty"}, "\a"},
		{"kitty", map[string]string{"TERM": "xterm-kitty"}, "\x1b]99;;Done\x07"},
		{"wezterm", map[string]string{"TERM_PROGRAM": "WezTerm"}, "\x1b]9;Done\x07"},
		{"other terminal", map[string]string{"TERM": "xterm"}, "\a"},
	}
	for _, c := range cases {
		m := playTerminal(newModel(nil, Options{}, nil))
		m.screen.notifications = true
		m.screen.focused = false
		m.screen.env = envOf(c.env)
		if got := flatten(m.notify("Done")); !reflect.DeepEqual(got, []tea.Msg{tea.RawMsg{Msg: c.want}}) {
			t.Errorf("%s: notify = %#v, want %q", c.name, got, c.want)
		}
	}
}

func kittyBlurred(t *testing.T) model {
	t.Helper()
	m := playTerminal(newModel(nil, Options{}, nil))
	m.conn.product = "dalgona"
	m.screen.notifications = true
	m.screen.env = envOf(map[string]string{"TERM": "xterm-kitty"})
	m.thread.id = "t1"
	m.thread.title = "Plan"
	m, _ = drive(t, m, tea.BlurMsg{})
	return m
}

func TestTurnEndNotifiesWhenBlurred(t *testing.T) {
	t.Parallel()
	m := kittyBlurred(t)
	command := m.applyNotification(notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"completed"}}`)})
	want := tea.RawMsg{Msg: "\x1b]99;;dalgona: Done · Plan\x07"}
	if !containsMsg(flatten(command), want) {
		t.Fatalf("turn end notification = %#v, want %#v", flatten(command), want)
	}
}

func TestTurnEndWordFollowsStatus(t *testing.T) {
	t.Parallel()
	cases := []struct{ status, word string }{{"completed", "Done"}, {"failed", "Failed"}, {"interrupted", "Stopped"}}
	for _, c := range cases {
		m := kittyBlurred(t)
		command := m.applyNotification(notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"` + c.status + `"}}`)})
		want := tea.RawMsg{Msg: "\x1b]99;;dalgona: " + c.word + " · Plan\x07"}
		if !containsMsg(flatten(command), want) {
			t.Errorf("%s: notification = %#v, want %#v", c.status, flatten(command), want)
		}
	}
}

func TestApprovalRequestNotifiesWhenBlurred(t *testing.T) {
	t.Parallel()
	m := kittyBlurred(t)
	command := m.applyRequest(serverRequestMsg{
		method: "item/commandExecution/requestApproval", answer: make(chan requestAnswer, 1),
		params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"c1","startedAtMs":1,"command":"go test ./...","reason":"verify"}`),
	})
	want := tea.RawMsg{Msg: "\x1b]99;;dalgona: Waiting for approval · Plan\x07"}
	if !containsMsg(flatten(command), want) {
		t.Fatalf("approval notification = %#v, want %#v", flatten(command), want)
	}
}

func TestNotifyIgnoresOtherThreads(t *testing.T) {
	t.Parallel()
	m := kittyBlurred(t)
	command := m.applyNotification(notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t2","turn":{"id":"turn-2","items":[],"status":"completed"}}`)})
	if hasRaw(flatten(command)) {
		t.Fatal("turn end of another thread sent a desktop notification")
	}
	command = m.applyRequest(serverRequestMsg{
		method: "item/commandExecution/requestApproval", answer: make(chan requestAnswer, 1),
		params: []byte(`{"threadId":"t2","turnId":"turn-2","itemId":"c2","startedAtMs":1,"command":"go test ./...","reason":"verify"}`),
	})
	if hasRaw(flatten(command)) {
		t.Fatal("approval of another thread sent a desktop notification")
	}
}

func hasRaw(messages []tea.Msg) bool {
	for _, message := range messages {
		if _, ok := message.(tea.RawMsg); ok {
			return true
		}
	}
	return false
}

func TestLoadThreadPrintsSeparatorOnly(t *testing.T) {
	t.Parallel()
	m := playTerminal(newModel(nil, Options{}, nil))
	m.screen.width = 80
	items := []rpc.Item{{ID: "c1", Type: "commandExecution", Command: "ls", Status: "completed"}, {ID: "c2", Type: "commandExecution", Command: "pwd", Status: "completed"}}
	m, _ = drive(t, m, resultMsg{op: "resumed", value: &resumedThread{Thread: rpc.Thread{ID: "t1", Name: "Plan", Turns: []rpc.Turn{{ID: "turn-1", Items: items, Status: "completed"}}}}})
	before := len(m.out.entries)
	m, _ = drive(t, m, resultMsg{op: "resumed", value: &resumedThread{Thread: rpc.Thread{ID: "t2", Name: "Plan"}}})
	for _, key := range []string{"thread:t1:1", fixtureItemKey("c1"), fixtureItemKey("c2"), "thread:t2:2"} {
		if _, ok := m.out.printed[key]; !ok {
			t.Fatalf("key %s not printed; printed %v", key, m.out.printed)
		}
	}
	if len(m.out.entries) <= before {
		t.Fatal("second load added no entry")
	}
	if got := threadRowText(m, "thread:t2:2"); got != "── Plan ──" {
		t.Fatalf("thread row text = %q, want %q", got, "── Plan ──")
	}
}

func TestThreadRowKeysCountLoads(t *testing.T) {
	t.Parallel()
	m := playTerminal(newModel(nil, Options{}, nil))
	m.screen.width = 80
	for _, id := range []string{"t1", "t2", "t1"} {
		m, _ = drive(t, m, resultMsg{op: "resumed", value: &resumedThread{Thread: rpc.Thread{ID: id, Name: "Plan"}}})
	}
	for _, key := range []string{"thread:t1:1", "thread:t2:2", "thread:t1:3"} {
		if got := threadRowText(m, key); got != "── Plan ──" {
			t.Fatalf("%s text = %q, want %q", key, got, "── Plan ──")
		}
	}
	m = playTerminal(newModel(nil, Options{}, nil))
	m.screen.width = 80
	for _, id := range []string{"t1", "t1"} {
		m, _ = drive(t, m, resultMsg{op: "resumed", value: &resumedThread{Thread: rpc.Thread{ID: id, Name: "Plan"}}})
	}
	if _, ok := m.out.printed["thread:t1:1"]; !ok {
		t.Fatal("first load of t1 printed no thread row")
	}
	if _, ok := m.out.printed["thread:t1:2"]; ok {
		t.Fatal("reload of the followed thread printed a thread row")
	}
}

func containsMsg(messages []tea.Msg, want tea.Msg) bool {
	for _, message := range messages {
		if reflect.DeepEqual(message, want) {
			return true
		}
	}
	return false
}

func threadRowText(m model, key string) string {
	for _, entry := range m.out.entries {
		if entry.key == key {
			return entry.text
		}
	}
	return ""
}

func envOf(values map[string]string) tea.EnvMsg {
	env := make(tea.EnvMsg, 0, len(values))
	for key, value := range values {
		env = append(env, key+"="+value)
	}
	return env
}
