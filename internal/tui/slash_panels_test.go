package tui

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func panelPicker(t *testing.T, client caller) model {
	t.Helper()
	m := newTestModel(client, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m.thread.id = "t1"
	return m
}

func advisorPanelOpen(t *testing.T, m model, states []rpc.AdvisorState) model {
	t.Helper()
	m, _ = drive(t, m, resultMsg{op: "advisorPanel", value: &states})
	if m.list == nil || m.list.kind != "advisor" {
		t.Fatal("advisor panel did not open")
	}
	return m
}

func TestNewOpensConfirmPanel(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := panelPicker(t, caller)
	m.queue = []string{"first", "second"}
	if command := m.executeSlash("new", ""); command != nil || len(caller.calls) != 0 {
		t.Fatal("/new must open the confirm panel before it starts a thread")
	}
	assertContains(t, m.View().Content, "Start a new thread?", "2 follow-ups move to it")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	m = runCommand(t, m, command, "confirm produced no command")
	if last := caller.calls[len(caller.calls)-1]; last.method != "thread/start" {
		t.Fatalf("confirm called %s, want thread/start", last.method)
	}
}

func TestForkPanelSendsLastTurnId(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := panelPicker(t, caller)
	command := m.executeSlash("fork", "")
	if command == nil {
		t.Fatal("/fork produced no tree request")
	}
	_ = command()
	if caller.calls[0].method != "dalgona/thread/tree" {
		t.Fatalf("/fork called %s, want dalgona/thread/tree", caller.calls[0].method)
	}
	turns := []rpc.TreeTurn{
		{TurnID: "turn-8", Preview: "old", Active: true},
		{TurnID: "turn-9", Preview: "work", Active: true},
		{TurnID: "turn-x", Preview: "side", Active: false},
	}
	m, _ = drive(t, m, resultMsg{op: "forkTree", value: &rpc.ThreadTreeResponse{Turns: turns}})
	m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
	m = runCommand(t, m, command, "fork pick produced no command")
	last := caller.calls[len(caller.calls)-1]
	params, ok := last.params.(rpc.ThreadForkParams)
	if last.method != "thread/fork" || !ok || params.LastTurnID != "turn-9" {
		t.Fatalf("fork call = %#v, want thread/fork at the newest active turn turn-9", last)
	}
}

func TestAdvisorPanelToggles(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := advisorPanelOpen(t, panelPicker(t, caller), []rpc.AdvisorState{{Name: "advisor", Status: "disabled", Model: "gpt-test"}})
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	m = runCommand(t, m, command, "toggle produced no command")
	last := caller.calls[len(caller.calls)-1]
	set, ok := last.params.(rpc.DalgonaAdvisorSetParams)
	if last.method != "dalgona/advisor/set" || !ok || !set.Enabled || set.ThreadID != "t1" {
		t.Fatalf("toggle call = %#v, want enabled true on the disabled advisor", last)
	}
	if !strings.Contains(m.notice, "advisor on") || strings.Contains(m.notice, "advisor off") {
		t.Fatalf("enable notice = %q, want advisor on only", m.notice)
	}
	if m.list != nil {
		t.Fatalf("advisor list open after a toggle, want it closed")
	}
}

func TestAdvisorPanelShowsUnavailable(t *testing.T) {
	t.Parallel()
	m := advisorPanelOpen(t, panelPicker(t, new(fakeCaller)), []rpc.AdvisorState{{Name: "advisor", Status: "idle"}})
	unavailable := fmt.Errorf("rpc error -32001: %w", &rpc.RPCError{Code: -32001, Message: "advisor runtime is not available"})
	m, _ = drive(t, m, resultMsg{op: "advisorSet", err: unavailable})
	if m.list == nil {
		t.Fatal("the advisor panel closed on a failed toggle")
	}
	assertContains(t, m.list.err, "advisor unavailable: advisor runtime is not available", "config.toml")
	assertContains(t, m.View().Content, "advisor unavailable: advisor runtime is not available")
}

func TestAdvisorDockSteady(t *testing.T) {
	t.Parallel()
	m := advisorPanelOpen(t, panelPicker(t, new(fakeCaller)), []rpc.AdvisorState{{Name: "advisor", Status: "idle"}})
	if got := len(m.dockLines()); got != 15 {
		t.Fatalf("dock with the advisor panel open has %d lines, want 15", got)
	}
	keys := make([]tea.KeyPressMsg, 50)
	for i := range keys {
		keys[i] = tea.KeyPressMsg{Code: tea.KeyDown}
	}
	assertSteadyDock(t, m, keys)
}

func TestAdvisorResetNeedsConfirmation(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := advisorPanelOpen(t, panelPicker(t, caller), []rpc.AdvisorState{{Name: "advisor", Status: "idle"}})
	m, _ = drive(t, m, press('r', tea.ModCtrl, ""))
	assertContains(t, m.View().Content, "Reset the advisor?", "[enter] reset")
	before := len(caller.calls)
	_, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	if len(caller.calls) != before {
		t.Fatal("ctrl+r must open the reset confirm before it sends a request")
	}
	runCommand(t, m, command, "reset confirm produced no command")
	if last := caller.calls[len(caller.calls)-1]; last.method != "dalgona/advisor/reset" {
		t.Fatalf("reset called %s, want dalgona/advisor/reset", last.method)
	}
}

func TestReviewUncommittedSendsTarget(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := panelPicker(t, caller)
	if command := m.executeSlash("review", ""); command != nil || m.list == nil || m.list.kind != "review" {
		t.Fatal("/review must open the review panel without a request")
	}
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	m = runCommand(t, m, command, "uncommitted review produced no command")
	last := caller.calls[len(caller.calls)-1]
	params, ok := last.params.(rpc.ReviewStartParams)
	if last.method != "review/start" || !ok || params.ThreadID != "t1" || params.Target.Type != "uncommittedChanges" {
		t.Fatalf("review call = %#v, want review/start of uncommittedChanges on t1", last)
	}
}

func TestReviewBaseBranchTypedInInputLine(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := panelPicker(t, caller)
	m.executeSlash("review", "")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	if command != nil || len(caller.calls) != 0 {
		t.Fatal("choosing Base branch must wait for the branch name")
	}
	for _, r := range "main" {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	assertContains(t, m.View().Content, "branch: main")
	m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
	m = runCommand(t, m, command, "branch review produced no command")
	last := caller.calls[len(caller.calls)-1]
	params, ok := last.params.(rpc.ReviewStartParams)
	if last.method != "review/start" || !ok || params.Target.Type != "baseBranch" || params.Target.Branch != "main" {
		t.Fatalf("review call = %#v, want baseBranch main", last)
	}
}

func TestReviewRefusedMidTurn(t *testing.T) {
	t.Parallel()
	tests := []struct {
		state string
		word  string
	}{
		{state: stateRunning, word: "working"},
		{state: stateAwaitingApproval, word: "waiting for approval"},
		{state: "awaiting input", word: "waiting for an answer"},
	}
	for _, test := range tests {
		caller := new(fakeCaller)
		m := panelPicker(t, caller)
		m.thread.state = test.state
		if command := m.executeSlash("review", ""); command != nil || len(caller.calls) != 0 || m.list != nil {
			t.Fatalf("/review while %s opened a panel or sent a request", test.state)
		}
		assertContains(t, m.notice, "/review is unavailable while a turn is "+test.word+"; wait for it to finish or interrupt it first")
	}
}
