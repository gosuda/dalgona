package tui

import (
	"errors"
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/colorprofile"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/rpc"
)

func playTerminal(m model) model {
	if !m.out.started && m.screen.height == 0 {
		m.screen.height = 24
	}
	if m.screen.color && m.screen.profile == colorprofile.ASCII {
		m.screen.profile = colorprofile.TrueColor
	}
	m.out.started = true
	m.out.geometry = m.currentGeometry()
	return m
}

type flushMsg struct{}

func settleFrames(m model) model {
	for range 200 {
		updated, _ := m.Update(flushMsg{})
		m = updated.(model)
		var msg tea.Msg
		switch {
		case m.out.settling:
			msg = settledMsg{n: m.out.settleN}
		case m.out.printing:
			msg = printedMsg{}
		default:
			return m
		}
		updated, _ = m.Update(msg)
		m = updated.(model)
	}
	return m
}

func rawUpdate(t *testing.T, m model, msg tea.Msg) model {
	t.Helper()
	updated, _ := m.Update(msg)
	next, ok := updated.(model)
	if !ok {
		t.Fatalf("Update returned %T, want model", updated)
	}
	return next
}

func newTestModel(client caller, opts Options, done <-chan struct{}) model {
	return playTerminal(newModel(client, opts, done))
}

func lastEntry(t *testing.T, m model) string {
	t.Helper()
	if len(m.out.entries) == 0 {
		t.Fatal("no row was committed")
	}
	return m.out.entries[len(m.out.entries)-1].text
}

func assertFrame(t *testing.T, lines []string, width, height int) {
	t.Helper()
	if len(lines) > height-1 {
		t.Fatalf("frame has %d lines, want at most %d:\n%s", len(lines), height-1, strings.Join(lines, "\n"))
	}
	for _, line := range lines {
		if cells := ansi.StringWidth(line); cells > width-1 {
			t.Fatalf("line has %d cells, want at most %d: %q", cells, width-1, line)
		}
	}
}

func assertSteadyDock(t *testing.T, m model, keys []tea.KeyPressMsg) model {
	t.Helper()
	want := len(m.dockLines())
	for _, key := range keys {
		m, _ = drive(t, m, key)
		if got := len(m.dockLines()); got != want {
			t.Fatalf("dock height changed from %d to %d after %q", want, got, key.String())
		}
	}
	return m
}

func scriptedTurn(t *testing.T, m model) model {
	t.Helper()
	m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
	m, _ = drive(t, m, notificationMsg{method: "turn/started", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/started", params: []byte(`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"agentMessage","id":"a1","text":""}}`)})
	var text strings.Builder
	text.WriteString("# Plan\n\n")
	for i := range 50 {
		fmt.Fprintf(&text, "line %d of the answer that keeps streaming\n", i)
	}
	text.WriteString("```go\nfunc f() {\n}\n```\n")
	m, _ = drive(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(fmt.Sprintf(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":%q}`, text.String()))})
	m, _ = drive(t, m, notificationMsg{method: "item/completed", params: []byte(`{"threadId":"t1","turnId":"turn-1","completedAtMs":2,"item":{"type":"agentMessage","id":"a1","text":""}}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/started", params: []byte(`{"threadId":"t1","turnId":"turn-1","startedAtMs":3,"item":{"type":"commandExecution","id":"c1","command":"seq 30","commandActions":[],"cwd":"/work","status":"inProgress","aggregatedOutput":null}}`)})
	output := strings.Repeat("row\n", 30)
	m, _ = drive(t, m, notificationMsg{method: "item/commandExecution/outputDelta", params: []byte(fmt.Sprintf(`{"threadId":"t1","turnId":"turn-1","itemId":"c1","delta":%q}`, output))})
	m, _ = drive(t, m, notificationMsg{method: "item/completed", params: []byte(`{"threadId":"t1","turnId":"turn-1","completedAtMs":4,"item":{"type":"commandExecution","id":"c1","command":"seq 30","commandActions":[],"cwd":"/work","status":"completed","aggregatedOutput":"row","exitCode":0}}`)})
	m, _ = drive(t, m, notificationMsg{method: "error", params: []byte(`{"threadId":"t1","turnId":"turn-1","willRetry":true,"error":{"message":"stream reset"}}`)})
	m, _ = drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"completed"}}`)})
	return m
}

func TestDockBoundsOverScriptedTurn(t *testing.T) {
	t.Parallel()
	for _, size := range []struct{ width, height int }{{40, 8}, {100, 24}} {
		t.Run(fmt.Sprintf("%dx%d", size.width, size.height), func(t *testing.T) {
			t.Parallel()
			m := newTestModel(nil, Options{}, nil)
			m, _ = drive(t, m, tea.WindowSizeMsg{Width: size.width, Height: size.height})
			assertFrame(t, strings.Split(m.View().Content, "\n"), size.width, size.height)
			m = scriptedTurn(t, m)
			assertFrame(t, strings.Split(m.View().Content, "\n"), size.width, size.height)
		})
	}
}

func TestDockHeightChangesOnlyOnKeys(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	height := len(m.dockLines())
	m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":"one\ntwo\nthree\n"}`)})
	if got := len(m.dockLines()); got != height {
		t.Fatalf("dock height after streamed text = %d, want %d", got, height)
	}
	m, _ = drive(t, m, notificationMsg{method: "error", params: []byte(`{"threadId":"t1","turnId":"turn-1","willRetry":true,"error":{"message":"stream reset"}}`)})
	if got := len(m.dockLines()); got != height {
		t.Fatalf("dock height after notice = %d, want %d", got, height)
	}
	m, _ = drive(t, m, tea.PasteMsg{Content: "pasted\nline"})
	if got := len(m.dockLines()); got <= height {
		t.Fatalf("dock height after paste = %d, want more than %d", got, height)
	}
}

func TestNoticeIsOneRowAndKeepsDockHeight(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 24})
	before := len(m.dockLines())
	m.noticef("%s", strings.Repeat("notice ", 40))
	if got := len(m.dockLines()); got != before {
		t.Fatalf("dock height with a long notice = %d, want %d", got, before)
	}
}

func TestNoticeAndLiveTailBounded(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 8})
	m.noticef("%s", strings.Repeat("long notice ", 30))
	m.live.items = []liveItem{{Item: rpc.Item{ID: "a1", Type: "agentMessage", Text: strings.Repeat("tail ", 60)}}}
	assertFrame(t, strings.Split(m.View().Content, "\n"), 40, 8)
}

func TestApprovalCardKeepsTitleAndActionsInSmallWindow(t *testing.T) {
	t.Parallel()
	for _, size := range []struct{ width, height int }{{40, 12}, {100, 24}} {
		t.Run(fmt.Sprintf("%dx%d", size.width, size.height), func(t *testing.T) {
			t.Parallel()
			m := newTestModel(nil, Options{}, nil)
			m, _ = drive(t, m, tea.WindowSizeMsg{Width: size.width, Height: size.height})
			long := strings.Repeat("go test ./internal/tui/... -run TestFrame -count=1\n", 100)
			m, _ = drive(t, m, serverRequestMsg{
				method: "item/commandExecution/requestApproval",
				answer: make(chan requestAnswer, 1),
				params: []byte(fmt.Sprintf(`{"threadId":"t1","turnId":"turn-1","itemId":"cmd1","startedAtMs":1,"command":%q}`, long)),
			})
			lines := strings.Split(m.View().Content, "\n")
			assertFrame(t, lines, size.width, size.height)
			assertContains(t, strings.Join(lines, "\n"), "approval required", "[n] decline")
			before := strings.Join(lines, "\n")
			m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
			if strings.Join(strings.Split(m.View().Content, "\n"), "\n") == before {
				t.Fatal("down key did not scroll the approval body")
			}
		})
	}
}

func TestSelectorBoundedToWindow(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 60, Height: 24})
	models := make([]rpc.Model, 300)
	for i := range models {
		models[i] = rpc.Model{ID: fmt.Sprintf("model-%03d", i)}
	}
	m, _ = drive(t, m, resultMsg{op: "models", value: &rpc.ModelListResponse{Data: models}})
	m.selector.cursor = 250
	lines := strings.Split(m.View().Content, "\n")
	assertFrame(t, lines, 60, 24)
	assertContains(t, strings.Join(lines, "\n"), "model-250")
}

func TestSelectorDockSteady(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 60, Height: 24})
	choices := make([]choice, 40)
	for i := range choices {
		choices[i] = choice{id: fmt.Sprint(i), label: fmt.Sprint(i)}
	}
	m.selector = &selector{title: "models", choices: choices}
	keys := make([]tea.KeyPressMsg, 50)
	for i := range keys {
		keys[i] = tea.KeyPressMsg{Code: tea.KeyDown}
	}
	assertSteadyDock(t, m, keys)
}

func TestDockLeavesOneRow(t *testing.T) {
	t.Parallel()
	for _, height := range []int{4, 8, 24} {
		m := newTestModel(nil, Options{}, nil)
		m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: height})
		m.selector = &selector{title: "settings", choices: []choice{{id: "a", label: "a"}}}
		if got := len(m.dockLines()); got > height-1 {
			t.Errorf("height %d: dock has %d lines with a panel open", height, got)
		}
		m.selector = nil
		m.input.composer.SetValue(strings.Repeat("draft line\n", 20))
		if got := len(m.dockLines()); got > height-1 {
			t.Errorf("height %d: dock has %d lines with a long draft", height, got)
		}
	}
}

func TestEmptyRowPrintsAsSpace(t *testing.T) {
	t.Parallel()
	if got := chunkText([]string{""}); got != " " {
		t.Fatalf("chunkText of one empty row = %q, want a single space", got)
	}
}

func TestNoRowPrintsBeforeSize(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.commit("row", []string{"before the size"})
	m = rawUpdate(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
	if m.out.printing {
		t.Fatal("a row printed before the first window size")
	}
	if view := m.View().Content; view != "" {
		t.Fatalf("view before the first row printed = %q, want empty", view)
	}
}

func TestFirstViewIsEmptyUntilFirstRowPrints(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m = settleFrames(m)
	m.commit("thread:t1:1", []string{"started demo"})
	m = rawUpdate(t, m, tea.FocusMsg{})
	if m.out.printing && m.View().Content != "" {
		t.Fatalf("view during the first print = %q, want empty", m.View().Content)
	}
	m = settleFrames(m)
	if m.View().Content == "" {
		t.Fatal("view stayed empty after the first row printed")
	}
}

func TestViewIsFrozenWhilePrintInFlight(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m = settleFrames(m)
	m.commit("thread:t1:1", []string{"started demo"})
	m = settleFrames(m)
	if !m.out.started {
		t.Fatal("first row did not print")
	}
	m = rawUpdate(t, m, notificationMsg{method: "item/completed", params: []byte(`{"threadId":"t1","turnId":"turn-1","completedAtMs":2,"item":{"type":"commandExecution","id":"i1","command":"ls","commandActions":[],"cwd":"/w","status":"completed","aggregatedOutput":"a"}}`)})
	m = settleFrames(m)
	frame := len(m.dockLines())
	long := strings.Repeat("line\n", 299) + "line"
	m = rawUpdate(t, m, notificationMsg{method: "item/completed", params: []byte(fmt.Sprintf(`{"threadId":"t1","turnId":"turn-1","completedAtMs":3,"item":{"type":"commandExecution","id":"i2","command":"seq","commandActions":[],"cwd":"/w","status":"completed","aggregatedOutput":%q}}`, long))})
	if !m.out.printing {
		t.Fatal("completed item did not start a print")
	}
	m.selector = &selector{title: "models", choices: []choice{{id: "a", label: "a"}}}
	m = rawUpdate(t, m, tea.FocusMsg{})
	if got := len(strings.Split(m.View().Content, "\n")); got != frame {
		t.Fatalf("frozen view has %d rows during the print, want %d", got, frame)
	}
	m = rawUpdate(t, m, printedMsg{})
	if !m.out.settling {
		t.Fatal("panel change after the print did not wait for a settled frame")
	}
	m = settleFrames(m)
	if got := len(m.dockLines()); got != 15 {
		t.Fatalf("dock with a list panel has %d rows, want 15", got)
	}
}

func TestPrintWaitsForSettledFrame(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m = settleFrames(m)
	m.commit("thread:t1:1", []string{"started demo"})
	m = settleFrames(m)
	m = rawUpdate(t, m, resultMsg{op: "models", value: &rpc.ModelListResponse{Data: []rpc.Model{{ID: "one"}}}})
	m = settleFrames(m)
	m = rawUpdate(t, m, press(tea.KeyEscape, 0, ""))
	m = rawUpdate(t, m, notificationMsg{method: "item/completed", params: []byte(`{"threadId":"t1","turnId":"turn-1","completedAtMs":2,"item":{"type":"commandExecution","id":"i1","command":"ls","commandActions":[],"cwd":"/w","status":"completed","aggregatedOutput":"a"}}`)})
	if m.out.printing || !m.out.settling {
		t.Fatalf("row printed on an unsettled frame: printing %v settling %v", m.out.printing, m.out.settling)
	}
	m = rawUpdate(t, m, settledMsg{n: m.out.settleN})
	if !m.out.printing {
		t.Fatal("row did not print after the frame settled")
	}
}

func TestCommitInGeometryChangeWaitsForSettle(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m = settleFrames(m)
	m.commit("thread:t1:1", []string{"started demo"})
	m = settleFrames(m)
	m.selector = &selector{title: "models", choices: []choice{{id: "a", label: "a"}}}
	m.commit("k1", []string{"row"})
	m = rawUpdate(t, m, tea.FocusMsg{})
	if m.out.printing || !m.out.settling {
		t.Fatalf("print started during a geometry change: printing %v settling %v", m.out.printing, m.out.settling)
	}
	m = rawUpdate(t, m, settledMsg{n: m.out.settleN})
	if !m.out.printing {
		t.Fatal("row did not print after the frame settled")
	}
}

func TestEditorRowsKeepCaretRow(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m.screen.width = 40
	var lines []string
	for i := range 20 {
		lines = append(lines, fmt.Sprintf("line %02d", i))
	}
	m.input.composer.SetValue(strings.Join(lines, "\n"))
	m.input.composer.cursor = 0
	rows := m.editorRows(39, 3)
	if len(rows) != 3 || !strings.Contains(rows[0], "line 00") {
		t.Fatalf("editor rows with the caret on line 00 = %q", rows)
	}
}

func TestFailedInitializeCommitsErrorRow(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 100, Height: 8})
	m = rawUpdate(t, m, resultMsg{op: "initialize", err: errors.New("dial failed\nretry later")})
	m = settleFrames(m)
	if !m.out.started {
		t.Fatal("a failed start printed no row")
	}
	assertContains(t, lastEntry(t, m), "Could not start: dial failed retry later. Press ctrl+c to exit.")
}
