package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func revertPicker(t *testing.T, client caller, preview string) model {
	t.Helper()
	m := newTestModel(client, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 24})
	m.thread.id = "t1"
	m, _ = drive(t, m, resultMsg{op: "revertTree", value: &rpc.ThreadTreeResponse{Turns: []rpc.TreeTurn{{TurnID: "turn-9", Preview: preview, Active: true}}}})
	return m
}

func TestRevertNeedsConfirmation(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := revertPicker(t, caller, "work")
	before := len(caller.calls)
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if len(caller.calls) != before {
		t.Fatalf("enter on the picker sent %d calls, want none", len(caller.calls)-before)
	}
	assertContains(t, m.View().Content, "Revert to before this turn?")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	runCommand(t, m, command, "confirm produced no command")
	last := caller.calls[len(caller.calls)-1]
	params, ok := last.params.(rpc.ThreadRevertParams)
	if last.method != "thread/revert" || !ok || params.BeforeTurnID != "turn-9" {
		t.Fatalf("revert call = %#v", last)
	}
}

func TestRevertPreviewIsOneRow(t *testing.T) {
	t.Parallel()
	m := revertPicker(t, new(fakeCaller), "a\nb")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	found := false
	for line := range strings.SplitSeq(m.View().Content, "\n") {
		if strings.Contains(line, "`a b`") {
			found = true
		}
		if strings.TrimLeft(line, "│┃ ") == "b" {
			t.Fatalf("preview wrapped into its own row:\n%s", m.View().Content)
		}
	}
	if !found {
		t.Fatalf("preview row missing:\n%s", m.View().Content)
	}
}

func TestConfirmPanelBodyKeepsFourRows(t *testing.T) {
	t.Parallel()
	m := revertPicker(t, new(fakeCaller), "work")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	lines := strings.Split(m.View().Content, "\n")
	title := -1
	for i, line := range lines {
		if strings.Contains(line, "Revert to before this turn?") {
			title = i
		}
	}
	if title < 0 || title+5 >= len(lines) {
		t.Fatalf("confirm title or action row missing:\n%s", m.View().Content)
	}
	if !strings.Contains(lines[title+5], "[enter] revert") {
		t.Fatalf("action row = %q, want the revert and keep actions", lines[title+5])
	}
}

func TestConfirmPanelSteady(t *testing.T) {
	t.Parallel()
	m := revertPicker(t, new(fakeCaller), "work")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	keys := make([]tea.KeyPressMsg, 50)
	for i := range keys {
		keys[i] = tea.KeyPressMsg{Code: tea.KeyDown}
	}
	assertSteadyDock(t, m, keys)
}

func TestConfirmPanelNamesLayoutAndTurn(t *testing.T) {
	t.Parallel()
	m := revertPicker(t, new(fakeCaller), "work")
	m.openRevert("turn-9", "work")
	panel := m.confirmPanel()
	if panel.layout != panelConfirm || panel.kind != "revert" || panel.id != "turn-9" || !panel.decision {
		t.Fatalf("confirm panel = kind %q id %q layout %v decision %v", panel.kind, panel.id, panel.layout, panel.decision)
	}
}
