package tui

import (
	"context"
	"errors"
	"slices"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

type callRecord struct {
	method string
	params any
}

type fakeCaller struct {
	calls []callRecord
}

func (f *fakeCaller) Notify(method string, params any) error {
	f.calls = append(f.calls, callRecord{method: method, params: params})
	return nil
}

func (f *fakeCaller) Call(_ context.Context, method string, params, result any) error {
	f.calls = append(f.calls, callRecord{method: method, params: params})
	switch out := result.(type) {
	case *rpc.TurnStartResponse:
		out.Turn = rpc.Turn{ID: "turn-1", Status: "inProgress"}
	case *rpc.ThreadResumeResponse:
		out.Thread = rpc.Thread{ID: "t1", Name: "resumed"}
		out.Model = "local/qwen-test"
	case *rpc.ThreadReadResponse:
		out.Thread = rpc.Thread{ID: "t1", Name: "resumed"}
	case *[]rpc.AdvisorState:
		*out = []rpc.AdvisorState{{Name: "advisor", Model: "gpt-test", Status: "idle", Notes: 2}}
	}
	return nil
}

func TestModelInitializeHandshake(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	if msg, ok := m.initialize()().(resultMsg); !ok || msg.err != nil {
		t.Fatalf("initialize result = %#v", msg)
	}
	if len(caller.calls) != 2 || caller.calls[0].method != "initialize" || caller.calls[1].method != "initialized" {
		t.Fatalf("handshake calls = %#v", caller.calls)
	}
	params, ok := caller.calls[0].params.(rpc.InitializeParams)
	if !ok || params.ClientInfo.Name != "dalgona" {
		t.Fatalf("initialize params = %#v", caller.calls[0].params)
	}
}

func TestModelShowsConnectionLoss(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.thread.id = "t1"
	m, _ = drive(t, m, disconnectedMsg{err: errors.New("EOF")})
	m, _ = drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"x","items":[],"status":"completed"}}`)})
	assertContains(t, strings.Join(strings.Fields(m.View().Content), " "), "Offline")
	assertShown(t, m, "connection lost: EOF; restart the TUI, then /resume t1")
}

func drive(t *testing.T, m model, msg tea.Msg) (model, tea.Cmd) {
	t.Helper()
	if batch, ok := msg.(tea.BatchMsg); ok {
		for _, command := range batch {
			if command != nil {
				m, _ = drive(t, m, command())
			}
		}
		return m, nil
	}
	m = playTerminal(m)
	updated, command := m.Update(msg)
	next, ok := updated.(model)
	if !ok {
		t.Fatalf("Update returned %T, want model", updated)
	}
	return settleFrames(next), command
}

func press(code rune, mod tea.KeyMod, text string) tea.KeyPressMsg {
	return tea.KeyPressMsg{Code: code, Mod: mod, Text: text}
}

func TestModelStreamsItemsAndShowsStates(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 80
	m.input.composer.SetWidth(80)
	m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/started", params: []byte(`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"agentMessage","id":"a1","text":""}}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":"hel"}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":"lo"}`)})
	view := m.View().Content
	for _, want := range []string{"hello", "Working", "default ⊡"} {
		if !strings.Contains(view, want) {
			t.Errorf("streaming view missing %q:\n%s", want, view)
		}
	}
	m, command := drive(t, m, notificationMsg{method: "item/completed", params: []byte(`{"threadId":"t1","turnId":"turn-1","completedAtMs":2,"item":{"type":"agentMessage","id":"a1","text":""}}`)})
	if command == nil {
		t.Fatal("completed item did not schedule tea.Println")
	}
	m, _ = drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"interrupted"}}`)})
	if !strings.Contains(m.View().Content, "Stopped") {
		t.Fatalf("interrupted state is not visible:\n%s", m.View().Content)
	}
	m.thread.sandbox = "off"
	m, _ = drive(t, m, notificationMsg{method: "error", params: []byte(`{"threadId":"t1","turnId":"turn-1","willRetry":false,"error":{"message":"provider unavailable"}}`)})
	view = strings.Join(strings.Fields(m.View().Content), " ")
	for _, want := range []string{"Failed", "⊘"} {
		if !strings.Contains(view, want) {
			t.Errorf("failure view missing %q:\n%s", want, view)
		}
	}
	assertContains(t, m.View().Content, "error: provider unav")
	assertContains(t, m.notice, "retry the turn")
}

func assertContains(t *testing.T, view string, wants ...string) {
	t.Helper()
	for _, want := range wants {
		if !strings.Contains(view, want) {
			t.Errorf("view missing %q:\n%s", want, view)
		}
	}
}

func runCommand(t *testing.T, m model, command tea.Cmd, missing string) model {
	t.Helper()
	if command == nil {
		t.Fatal(missing)
	}
	m, _ = drive(t, m, command())
	return m
}

func TestModelApprovalPromptAndDecision(t *testing.T) {
	t.Parallel()
	tests := []struct {
		key      rune
		decision string
	}{
		{key: 'y', decision: "accept"},
		{key: 'a', decision: "acceptForSession"},
		{key: 'n', decision: "decline"},
	}
	for _, test := range tests {
		t.Run(test.decision, func(t *testing.T) {
			t.Parallel()
			m := newModel(nil, Options{}, nil)
			answer := make(chan requestAnswer, 1)
			request := serverRequestMsg{
				method: "item/commandExecution/requestApproval",
				params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"cmd1","startedAtMs":1,"command":"go test ./..."}`),
				answer: answer,
			}
			m, _ = drive(t, m, request)
			assertContains(t, m.View().Content, "Approval · Run command", "go test ./...", "[y] accept", "[a] accept for session", "[n] decline", "Waiting")
			m, _ = drive(t, m, press(test.key, 0, string(test.key)))
			response := <-answer
			decision, ok := response.value.(rpc.CommandExecutionRequestApprovalResponse)
			if !ok || decision.Decision != test.decision || response.err != nil {
				t.Fatalf("approval response = %#v, %v; want %s", response.value, response.err, test.decision)
			}
			if m.thread.state != stateRunning {
				t.Fatalf("state after approval = %q, want running", m.thread.state)
			}
		})
	}
}

func TestModelPromptAndSteer(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.input.composer.SetValue("hello")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	if command == nil {
		t.Fatal("Enter did not send a prompt")
	}
	m, _ = drive(t, m, command())
	if got := caller.calls[0].method; got != "turn/start" {
		t.Fatalf("prompt method = %q, want turn/start", got)
	}
	if m.thread.turnID != "turn-1" {
		t.Fatalf("accepted turn id = %q, want turn-1", m.thread.turnID)
	}
	m.input.composer.SetValue("change course")
	m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
	if command == nil {
		t.Fatal("Enter while running did not steer")
	}
	m, _ = drive(t, m, command())
	steer, ok := caller.calls[1].params.(rpc.TurnSteerParams)
	if caller.calls[1].method != "turn/steer" || !ok || steer.ExpectedTurnID != "turn-1" {
		t.Fatalf("running prompt call = %#v, want turn/steer expecting turn-1", caller.calls[1])
	}
}

func TestModelFollowupAndInterrupt(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.thread.turnID = "turn-1"
	m.thread.state = stateRunning
	m.input.composer.SetValue("after this")
	m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	if len(m.queue) != 1 || m.queue[0] != "after this" {
		t.Fatalf("follow-up queue = %v, want [after this]", m.queue)
	}
	m, command := drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"completed"}}`)})
	m = runCommand(t, m, command, "completed turn did not start the queued follow-up")
	last := caller.calls[len(caller.calls)-1]
	params, ok := last.params.(rpc.TurnStartParams)
	if !ok || last.method != "turn/start" || params.Input[0].Text != "after this" {
		t.Fatalf("follow-up call = %#v, want turn/start after this", last)
	}
	m = firstEscArms(t, m, caller)
	m, command = drive(t, m, press(tea.KeyEscape, 0, ""))
	m = runCommand(t, m, command, "Esc did not interrupt the active turn")
	last = caller.calls[len(caller.calls)-1]
	if last.method != "turn/interrupt" || m.thread.state != stateInterrupted {
		t.Fatalf("interrupt = %q, state %q", last.method, m.thread.state)
	}
}

func firstEscArms(t *testing.T, m model, caller *fakeCaller) model {
	t.Helper()
	calls := len(caller.calls)
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if len(caller.calls) != calls {
		t.Fatalf("first Esc sent %q, want no request", caller.calls[len(caller.calls)-1].method)
	}
	if m.hint.text != "Esc again to interrupt" {
		t.Fatalf("first Esc hint = %q", m.hint.text)
	}
	return m
}

func TestModelSlashCommands(t *testing.T) {
	t.Parallel()
	direct := []struct {
		input  string
		method string
	}{
		{input: "/model", method: "model/list"},
		{input: "/model gpt-test", method: "config/value/write"},
		{input: "/resume", method: "thread/list"},
		{input: "/login openai", method: "dalgona/account/providers/list"},
		{input: "/tree", method: "dalgona/thread/tree"},
		{input: "/revert", method: "dalgona/thread/tree"},
		{input: "/fork", method: "dalgona/thread/tree"},
	}
	for _, test := range direct {
		t.Run(test.input, func(t *testing.T) {
			caller, m := slashModel()
			name, argument, _ := parseSlash(test.input)
			checkCallResult(t, caller, m.executeSlash(name, argument), test.method)
		})
	}
	confirmed := []struct {
		input  string
		method string
	}{
		{input: "/new", method: "thread/start"},
		{input: "/compact", method: "thread/compact/start"},
	}
	for _, test := range confirmed {
		t.Run(test.input, func(t *testing.T) {
			caller, m := slashModel()
			name, argument, _ := parseSlash(test.input)
			command := m.executeSlash(name, argument)
			if command != nil || m.confirm == nil {
				t.Fatal("slash command must open a confirm panel before its request")
			}
			checkCallResult(t, caller, m.confirmKey("enter"), test.method)
		})
	}
}

func slashModel() (*fakeCaller, model) {
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	return caller, m
}

func checkCallResult(t *testing.T, caller *fakeCaller, command tea.Cmd, method string) {
	t.Helper()
	if command == nil {
		t.Fatal("slash command produced no RPC command")
	}
	result := command()
	if len(caller.calls) != 1 || caller.calls[0].method != method {
		t.Fatalf("calls = %#v, want %s", caller.calls, method)
	}
	if _, ok := result.(resultMsg); !ok {
		t.Fatalf("command result = %T, want resultMsg", result)
	}
}

func TestModelSelectorActions(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m, _ = drive(t, m, resultMsg{op: "models", value: &rpc.ModelListResponse{Data: []rpc.Model{{ID: "one"}, {ID: "hidden", Hidden: true}, {ID: "two"}}}})
	m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	if command == nil {
		t.Fatal("model selection produced no command")
	}
	m, _ = drive(t, m, command())
	if m.thread.modelName != "two" || caller.calls[0].method != "config/value/write" {
		t.Fatalf("selected model = %q, calls %#v", m.thread.modelName, caller.calls)
	}
	m, _ = drive(t, m, resultMsg{op: "revertTree", value: &rpc.ThreadTreeResponse{Turns: []rpc.TreeTurn{{TurnID: "turn-8", Preview: "old"}, {TurnID: "turn-9", Preview: "work", Active: true}}}})
	assertContains(t, m.View().Content, "revert before a turn", "work")
	before := len(caller.calls)
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if len(caller.calls) != before {
		t.Fatalf("enter on the picker sent %d calls, want none before the confirm", len(caller.calls)-before)
	}
	assertContains(t, m.View().Content, "Revert to before this turn?")
	m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
	m = runCommand(t, m, command, "confirm produced no command")
	last := caller.calls[len(caller.calls)-1]
	params, ok := last.params.(rpc.ThreadRevertParams)
	if last.method != "thread/revert" || !ok || params.BeforeTurnID != "turn-9" {
		t.Fatalf("revert call = %#v", last)
	}
	assertShown(t, m, "files changed by later turns are not reverted")
	assertContains(t, m.notice, "files changed by later turns are not reverted")
}

func TestModelTreeSelectionSetsLeaf(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m, _ = drive(t, m, resultMsg{op: "tree", value: &rpc.ThreadTreeResponse{Turns: []rpc.TreeTurn{{TurnID: "turn-8", Preview: "old"}, {TurnID: "turn-9", Preview: "work", Active: true}}}})
	assertContains(t, m.View().Content, "choose the active branch", "old", "work (active)")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	m = runCommand(t, m, command, "tree selection produced no command")
	last := caller.calls[len(caller.calls)-1]
	params, ok := last.params.(rpc.ThreadLeafSetParams)
	if last.method != "dalgona/thread/leaf/set" || !ok || params.TurnID != "turn-8" {
		t.Fatalf("leaf call = %#v", last)
	}
	assertShown(t, m, "files changed by later turns are not reverted")
	assertContains(t, m.notice, "files changed by later turns are not reverted")
}

func TestModelRendersSettledAssistantMarkdown(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 40
	rendered := stripANSI(settledText(&m, rpc.Item{Type: "agentMessage", Text: "**bold** and `code`"}))
	if !strings.Contains(rendered, "**bold**") || !strings.Contains(rendered, "code") {
		t.Fatalf("settled markdown = %q, want the emphasis markers kept without colour", rendered)
	}
}

func TestComposerAcceptsShiftedText(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m, _ = drive(t, m, press('h', tea.ModShift, "H"))
	m, _ = drive(t, m, press('i', tea.ModCapsLock, "I"))
	if got := m.input.composer.Value(); got != "HI" {
		t.Fatalf("composer = %q, want HI", got)
	}
}

func TestModelSessionOptionsDriveStatusAndThreadStart(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newTestModel(caller, Options{Model: "openai/gpt-test", Approval: "yolo", Sandbox: "workspace-write", Cwd: "/repo/checkout"}, nil)
	assertContains(t, m.View().Content, "openai/gpt-test ! ⊘ ▸ checkout")
	m.startThread()()
	params, ok := caller.calls[0].params.(rpc.ThreadStartParams)
	if !ok || params.Model != "openai/gpt-test" || params.ApprovalPolicy != "never" || params.Sandbox != "danger-full-access" || params.Cwd != "/repo/checkout" {
		t.Fatalf("thread/start params = %#v", caller.calls[0].params)
	}
}

func TestModelHoldsFollowupAfterInterruptedTurn(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.thread.turnID = "turn-1"
	m.thread.state = stateRunning
	m.queue = []string{"later"}
	m, _ = drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"interrupted"}}`)})
	if len(caller.calls) != 0 {
		t.Fatalf("interrupted turn released follow-up: calls %#v", caller.calls)
	}
	assertContains(t, strings.Join(strings.Fields(m.View().Content), " "), "Stopped")
	assertShown(t, m, "Held · 1 · Enter on an empty prompt sends the next")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	runCommand(t, m, command, "empty Enter did not release the held follow-up")
	params, ok := caller.calls[0].params.(rpc.TurnStartParams)
	if !ok || params.Input[0].Text != "later" {
		t.Fatalf("released follow-up = %#v", caller.calls[0])
	}
}

func TestModelDoubleCtrlCAndNarrowWrap(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.screen.width = 40
	m.input.composer.SetWidth(40)
	m.thread.title = "a thread with a long visible title"
	m.live.items = []liveItem{{Item: rpc.Item{ID: "a1", Type: "agentMessage", Text: strings.Repeat("wrappable ", 20)}}}
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	if !strings.Contains(m.hint.text, "again") {
		t.Fatalf("first Ctrl+C hint %q", m.hint.text)
	}
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	if !m.quitting {
		t.Fatal("second Ctrl+C did not quit")
	}
	command := m.finishQuit()
	if command == nil {
		t.Fatal("quit did not finish after the rows printed")
	}
	if _, ok := command().(tea.QuitMsg); !ok {
		t.Fatalf("quit returned %T, want tea.QuitMsg", command())
	}
	for line := range strings.SplitSeq(m.View().Content, "\n") {
		if width := lipgloss.Width(line); width > 40 {
			t.Errorf("line width = %d, want <= 40: %q", width, line)
		}
	}
}

func TestModelReportsMalformedProtocolWithRecovery(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.thread.id = "t1"
	m, _ = drive(t, m, notificationMsg{method: "item/started", params: []byte(`[]`)})
	if !strings.Contains(m.notice, "protocol error") || !strings.Contains(m.notice, "/resume t1") {
		t.Fatalf("malformed notification notice = %q", m.notice)
	}
}

func TestModelOpensInitialThread(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name     string
		threadID string
		methods  []string
	}{
		{name: "new", methods: []string{"thread/start"}},
		{name: "resume", threadID: "t2", methods: []string{"thread/resume", "thread/read"}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := newModel(caller, Options{ThreadID: test.threadID}, nil)
			m.startThread()()
			if len(caller.calls) != len(test.methods) {
				t.Fatalf("calls = %#v, want methods %v", caller.calls, test.methods)
			}
			for i, method := range test.methods {
				if caller.calls[i].method != method {
					t.Fatalf("call %d = %q, want %q", i, caller.calls[i].method, method)
				}
			}
		})
	}
}

func TestModelIgnoresStaleStatusAndInterruptAcknowledgement(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.thread.seq = 4
	m.thread.turnID = "turn-new"
	m.thread.state = stateRunning
	m, _ = drive(t, m, notificationMsg{method: "thread/status/changed", params: []byte(`{"threadId":"t1","status":{"type":"systemError"},"seq":3}`)})
	if m.thread.state != stateRunning || m.thread.seq != 4 {
		t.Fatalf("stale status changed state to %q seq %d", m.thread.state, m.thread.seq)
	}
	m, _ = drive(t, m, resultMsg{op: "interrupt", turnID: "turn-old"})
	if m.thread.state != stateRunning {
		t.Fatalf("stale interrupt changed state to %q", m.thread.state)
	}
	if len(caller.calls) != 0 {
		t.Fatalf("stale messages caused calls %#v", caller.calls)
	}
}

func TestModelResetsStatusSeqOnThreadChange(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m, _ = drive(t, m, notificationMsg{method: "thread/status/changed", params: []byte(`{"threadId":"t1","status":{"type":"idle"},"seq":9}`)})
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Thread: rpc.Thread{ID: "t2"}}})
	m, _ = drive(t, m, notificationMsg{method: "thread/status/changed", params: []byte(`{"threadId":"t2","status":{"type":"active","activeFlags":[]},"seq":2}`)})
	if m.thread.id != "t2" || m.thread.state != stateRunning || m.thread.seq != 2 {
		t.Fatalf("after /new: thread %q state %q seq %d", m.thread.id, m.thread.state, m.thread.seq)
	}
}

func TestModelFailurePreservesComposerAndSteerState(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.thread.id = "t1"
	m.thread.state = stateRunning
	m.input.composer.SetValue("new text")
	m, _ = drive(t, m, resultMsg{op: "steer", text: "old text", err: errors.New("closed")})
	if got := m.input.composer.Value(); got != "old text\nnew text" {
		t.Fatalf("composer after steer failure = %q", got)
	}
	if m.thread.state != stateRunning {
		t.Fatalf("steer failure changed state to %q", m.thread.state)
	}
}

func TestNestedTaskRowIsSubagent(t *testing.T) {
	t.Parallel()
	m := notifyTurn(t, rowModel(t, 80), "turn/started", "inProgress")
	m = notifyItem(t, m, "item/started", subagentItem("e1/3", "inProgress", ""))
	assertContains(t, strings.Join(m.dockLines(), "\n"), "1 run")
	m = notifyItem(t, m, "item/completed", subagentItem("e1/3", "completed", `{"data":"done"}`))
	count := 0
	for _, key := range entryKeys(m) {
		if key == "item:t1:e1/3" {
			count++
		}
	}
	if count != 1 {
		t.Fatalf("subagent call committed %d rows, want 1", count)
	}
	assertContains(t, entryText(t, m, "item:t1:e1/3"), "Subagent")
}

func TestNestedTodoWritesPlanRowOnly(t *testing.T) {
	t.Parallel()
	m := notifyTurn(t, rowModel(t, 80), "turn/started", "inProgress")
	m = notifyItem(t, m, "item/completed", `{"type":"dynamicToolCall","id":"e1/2","tool":"todo","arguments":{"items":[{"text":"read the test","status":"completed"}]},"status":"completed"}`)
	if slices.Contains(entryKeys(m), "item:t1:e1/2") {
		t.Fatal("a successful nested todo committed its tool row")
	}
	m = planUpdate(t, m, `[{"step":"read the test","status":"completed"}]`)
	if got, want := planKeys(m), []string{"plan:turn-1:0"}; !slices.Equal(got, want) {
		t.Fatalf("plan keys = %v, want %v", got, want)
	}
	if first, _, _ := strings.Cut(entryText(t, m, "plan:turn-1:0"), "\n"); first != "Plan" {
		t.Fatalf("plan row starts with %q, want Plan", first)
	}
}

func TestNewThreadShowsServerModel(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Model: "local/qwen-test", Thread: rpc.Thread{ID: "t1"}}})
	if m.thread.modelName != "local/qwen-test" {
		t.Fatalf("model = %q, want the model the server reports", m.thread.modelName)
	}
	assertContains(t, dockText(m), "local/qwen-test")
}

func TestResumedThreadShowsServerModel(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, m.resumeThread("t1")())
	if m.thread.modelName != "local/qwen-test" {
		t.Fatalf("model = %q, want the model the server reports", m.thread.modelName)
	}
}
