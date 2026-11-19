package tui

import (
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestThreadCommandsRefusedMidTurn(t *testing.T) {
	t.Parallel()
	states := []struct {
		state string
		word  string
	}{
		{state: stateRunning, word: "working"},
		{state: stateAwaitingApproval, word: "waiting for approval"},
		{state: stateAwaitingInput, word: "waiting for an answer"},
	}
	for _, state := range states {
		for _, name := range []string{"new", "resume", "fork", "compact", "tree", "revert"} {
			t.Run(state.state+" /"+name, func(t *testing.T) {
				t.Parallel()
				assertRefused(t, name, state.state, state.word)
			})
		}
	}
}

func assertRefused(t *testing.T, name, state, word string) {
	t.Helper()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.thread.state = state
	if command := m.executeSlash(name, ""); command != nil {
		t.Fatalf("/%s produced an RPC command while a turn is %s", name, word)
	}
	if len(caller.calls) != 0 {
		t.Fatalf("/%s made calls while a turn is %s: %#v", name, word, caller.calls)
	}
	want := "/" + name + " is unavailable while a turn is " + word + "; wait for it to finish or interrupt it first"
	if m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
}

func TestThreadCommandsAllowedWhenIdle(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	if command := m.executeSlash("new", ""); command != nil || m.confirm == nil {
		t.Fatal("idle /new did not open the confirm panel")
	}
	if m.notice != "" {
		t.Fatalf("idle /new set notice %q", m.notice)
	}
}

func startTurn(t *testing.T, m model) model {
	t.Helper()
	m, _ = drive(t, m, notificationMsg{method: "turn/started", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)})
	if m.thread.state != stateRunning {
		t.Fatalf("thread state after turn/started = %q, want %q", m.thread.state, stateRunning)
	}
	return m
}

func busyNotice(name string) string {
	label := "/" + name
	if name == "rewind" {
		label = name
	}
	return label + " is unavailable while a turn is working; wait for it to finish or interrupt it first"
}

func panelModel(t *testing.T, caller *fakeCaller) model {
	t.Helper()
	m := newTestModel(caller, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 160, Height: 24})
	m.thread.id = "t1"
	return m
}

func assertRefusedOnEnter(t *testing.T, m model, caller *fakeCaller, name string) {
	t.Helper()
	before := len(caller.calls)
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	if command != nil {
		m, _ = drive(t, m, command())
	}
	if len(caller.calls) != before {
		t.Fatalf("enter sent calls while a turn was working: %#v", caller.calls[before:])
	}
	if m.notice != busyNotice(name) {
		t.Fatalf("notice = %q, want %q", m.notice, busyNotice(name))
	}
	assertContains(t, m.View().Content, busyNotice(name))
}

func TestConfirmRefusedWhenTurnStarted(t *testing.T) {
	t.Parallel()
	for _, name := range []string{"new", "compact", "revert", "rewind"} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := panelModel(t, caller)
			if name == "revert" || name == "rewind" {
				m, _ = drive(t, m, resultMsg{op: name + "Tree", value: &rpc.ThreadTreeResponse{Turns: []rpc.TreeTurn{{TurnID: "turn-9", Preview: "work", Active: true}}}})
				m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
			} else if command := m.executeSlash(name, ""); command != nil {
				t.Fatalf("/%s produced a command while idle", name)
			}
			if m.confirm == nil || m.confirm.kind != name {
				t.Fatalf("confirm after /%s = %#v, want kind %q", name, m.confirm, name)
			}
			m = startTurn(t, m)
			assertRefusedOnEnter(t, m, caller, name)
		})
	}
}

func TestPickerRefusedWhenTurnStarted(t *testing.T) {
	t.Parallel()
	turns := &rpc.ThreadTreeResponse{Turns: []rpc.TreeTurn{{TurnID: "turn-8", Preview: "old", Active: true}, {TurnID: "turn-9", Preview: "work", Active: true}}}
	threads := &rpc.ThreadListResponse{Data: []rpc.Thread{{ID: "t2", Name: "other work"}}}
	for _, test := range []struct {
		name string
		op   string
		view any
	}{
		{name: "resume", op: "threads", view: threads},
		{name: "tree", op: "tree", view: turns},
		{name: "fork", op: "forkTree", view: turns},
		{name: "revert", op: "revertTree", view: turns},
		{name: "rewind", op: "rewindTree", view: turns},
	} {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := panelModel(t, caller)
			m, _ = drive(t, m, resultMsg{op: test.op, value: test.view})
			if m.list == nil || m.list.kind != test.name {
				t.Fatalf("list after %s = %#v, want kind %q", test.op, m.list, test.name)
			}
			m = startTurn(t, m)
			assertRefusedOnEnter(t, m, caller, test.name)
		})
	}
}

func TestReviewRefusedWhenTurnStarted(t *testing.T) {
	t.Parallel()
	for _, test := range []struct {
		name  string
		steps func(t *testing.T, m model) model
	}{
		{name: "uncommitted changes", steps: func(t *testing.T, m model) model { return m }},
		{name: "base branch", steps: func(t *testing.T, m model) model {
			m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
			m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
			return typeText(t, m, "main")
		}},
		{name: "custom instructions", steps: func(t *testing.T, m model) model {
			m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
			m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
			m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
			m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
			return typeText(t, m, "look at the diff")
		}},
	} {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := panelModel(t, caller)
			if command := m.executeSlash("review", ""); command != nil || m.list == nil || m.list.kind != "review" {
				t.Fatal("/review must open the review panel without a request")
			}
			m = test.steps(t, m)
			m = startTurn(t, m)
			assertRefusedOnEnter(t, m, caller, "review")
		})
	}
}
