package tui

import (
	"slices"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
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

type confirmCase struct {
	name   string
	title  string
	notice string
	open   func(*model)
}

func TestConfirmPanelShowsItsBody(t *testing.T) {
	t.Parallel()
	cases := []confirmCase{
		{"revert", "Revert to before this turn?", revertNotice, func(m *model) { m.openRevert("turn-9", "work") }},
		{"rewind", "Rewind to before this turn?", rewindNotice, func(m *model) { m.openRewind("turn-9", "work") }},
	}
	for _, width := range []int{100, 40, 24} {
		for _, tc := range cases {
			checkConfirmBody(t, tc, width)
		}
	}
}

func confirmBodyRows(t *testing.T, tc confirmCase, width int) ([]string, string) {
	t.Helper()
	m := newTestModel(new(fakeCaller), Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: width, Height: 30})
	m.thread.id = "t1"
	tc.open(&m)
	content := stripANSI(m.View().Content)
	lines := strings.Split(content, "\n")
	title := slices.IndexFunc(lines, func(line string) bool { return strings.Contains(line, tc.title[:10]) })
	action := slices.IndexFunc(lines, func(line string) bool { return strings.HasPrefix(line, "└─ ") })
	if title < 0 || action <= title {
		t.Fatalf("%s at %d: confirm title or action row missing:\n%s", tc.name, width, content)
	}
	body := make([]string, 0, action-title-1)
	for _, row := range lines[title+1 : action] {
		body = append(body, strings.TrimLeft(strings.TrimRight(row, " "), "│ "))
	}
	return body, content
}

func checkConfirmBody(t *testing.T, tc confirmCase, width int) {
	t.Helper()
	body, content := confirmBodyRows(t, tc, width)
	cut := slices.ContainsFunc(body, func(row string) bool { return strings.HasSuffix(row, "...") })
	if cut && slices.Contains(body, "") {
		t.Errorf("%s at %d: a body row is cut while a body row stays empty:\n%s", tc.name, width, content)
	}
	if cut && width >= 40 {
		t.Errorf("%s at %d: the body is cut:\n%s", tc.name, width, content)
	}
	if cut {
		return
	}
	words := strings.Fields(strings.Join(body, " "))
	for _, word := range strings.Fields(tc.notice) {
		if !slices.Contains(words, word) {
			t.Errorf("%s at %d: word %q of the notice is not on screen:\n%s", tc.name, width, word, content)
		}
	}
}
