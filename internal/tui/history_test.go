package tui

import (
	"context"
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func typeText(t *testing.T, m model, text string) model {
	t.Helper()
	for _, r := range text {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	return m
}

func TestHistoryRecallAndDraftRestore(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m.thread.id = "t1"
	m = typeText(t, m, "a")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m = typeText(t, m, "dr")
	m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
	if got := m.input.composer.Value(); got != "a" {
		t.Fatalf("up recalled %q, want a", got)
	}
	m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
	if got := m.input.composer.Value(); got != "dr" {
		t.Fatalf("down restored %q, want dr", got)
	}
}

func TestLoginLineNotInHistory(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m.thread.id = "t1"
	m = typeText(t, m, "//login openai")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if n := len(m.input.history.entries); n != 0 {
		t.Fatalf("history holds %d entries after //login, want 0", n)
	}
	var h history
	h.add("/login x", "")
	if len(h.entries) != 0 {
		t.Fatalf("direct add of /login kept %d entries, want 0", len(h.entries))
	}
}

type numberedTurns struct {
	fakeCaller
	started int
}

func (c *numberedTurns) Call(ctx context.Context, method string, params, result any) error {
	out, ok := result.(*rpc.TurnStartResponse)
	if !ok {
		return c.fakeCaller.Call(ctx, method, params, result)
	}
	c.started++
	c.calls = append(c.calls, callRecord{method: method, params: params})
	out.Turn = rpc.Turn{ID: fmt.Sprintf("turn-%d", c.started), Status: "inProgress"}
	return nil
}

func TestResentPromptTakesNewTurn(t *testing.T) {
	t.Parallel()
	m := newModel(new(numberedTurns), Options{}, nil)
	m.thread.id = "t1"
	m = typeText(t, m, "hi")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, command())
	m, _ = drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"completed"}}`)})
	m = typeText(t, m, "hi")
	m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, command())
	if entries := m.input.history.entries; len(entries) != 1 || entries[0].turnID != "turn-2" {
		t.Fatalf("history = %#v, want one entry for turn-2", entries)
	}
}

func TestSkippedRepeatTakesGivenTurn(t *testing.T) {
	t.Parallel()
	var h history
	h.add("hi", "turn-1")
	h.add("hi", "turn-2")
	if len(h.entries) != 1 || h.entries[0].turnID != "turn-2" {
		t.Fatalf("entries = %#v, want one entry for turn-2", h.entries)
	}
}

func TestCtrlCSavesDraft(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m.thread.id = "t1"
	m = typeText(t, m, "dr")
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	if got := m.input.composer.Value(); got != "" {
		t.Fatalf("editor holds %q after Ctrl+C, want empty", got)
	}
	if n := len(m.input.history.entries); n != 1 || m.input.history.entries[0].text != "dr" {
		t.Fatalf("history = %#v, want the draft dr", m.input.history.entries)
	}
	if m.hint.text != "Draft saved to history · Ctrl+C again to exit" {
		t.Fatalf("hint = %q", m.hint.text)
	}
}

func TestHistoryRecallKeepsFrameInWindow(t *testing.T) {
	t.Parallel()
	m := newTestModel(new(fakeCaller), Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 8})
	m.thread.id = "t1"
	var lines []string
	for i := range 20 {
		lines = append(lines, fmt.Sprintf("line %02d", i))
	}
	m.input.history.add(strings.Join(lines, "\n"), "")
	m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
	frame := strings.Split(m.View().Content, "\n")
	assertFrame(t, frame, 40, 8)
}

func TestHistoryHoldsSentPromptsWithTurn(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m = typeText(t, m, "hello")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, command())
	if n := len(m.input.history.entries); n != 1 || m.input.history.entries[0].turnID != "turn-1" {
		t.Fatalf("history = %#v, want one entry for turn-1", m.input.history.entries)
	}
}

func TestHistoryDropsConsecutiveRepeats(t *testing.T) {
	t.Parallel()
	var h history
	h.add("same", "")
	h.add("same", "")
	h.add("other", "")
	h.add("same", "")
	if len(h.entries) != 3 {
		t.Fatalf("entries = %#v, want three", h.entries)
	}
}

func TestHistoryCapsAtTwoHundred(t *testing.T) {
	t.Parallel()
	var h history
	for i := range 205 {
		h.add(fmt.Sprintf("prompt %d", i), "")
	}
	if len(h.entries) != 200 || h.entries[0].text != "prompt 5" {
		t.Fatalf("entries = %d, first %q; want 200 starting at prompt 5", len(h.entries), h.entries[0].text)
	}
}

func TestHistoryFromUserMessageItem(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m.thread.id = "t1"
	m, _ = drive(t, m, notificationMsg{method: "item/completed", params: []byte(`{"threadId":"t1","item":{"id":"u1","type":"userMessage","text":"from the server"}}`)})
	if n := len(m.input.history.entries); n != 1 || m.input.history.entries[0].text != "from the server" {
		t.Fatalf("history = %#v, want the server message", m.input.history.entries)
	}
}

func TestHistoryFromResumedThread(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m.applyResumed(&resumedThread{Thread: rpc.Thread{ID: "t1", Turns: []rpc.Turn{{ID: "turn-1", Items: []rpc.Item{{ID: "u1", Type: "userMessage", Text: "earlier"}}}}}})
	if n := len(m.input.history.entries); n != 1 || m.input.history.entries[0].turnID != "turn-1" {
		t.Fatalf("history = %#v, want the resumed prompt with turn-1", m.input.history.entries)
	}
}
