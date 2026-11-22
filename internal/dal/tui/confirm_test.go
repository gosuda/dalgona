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
	lead   []string
	notice string
	open   func(*model)
}

func TestConfirmPanelShowsItsBody(t *testing.T) {
	t.Parallel()
	cases := []confirmCase{
		{"revert", "Revert to before this turn?", []string{"`work`"}, revertNotice, func(m *model) { m.openRevert("turn-9", "work") }},
		{"rewind", "Rewind to before this turn?", nil, rewindNotice, func(m *model) { m.openRewind("turn-9", "work") }},
	}
	for _, size := range [][2]int{{100, 30}, {40, 30}, {24, 30}, {100, 8}, {24, 8}} {
		for _, tc := range cases {
			checkConfirmBody(t, tc, size[0], size[1])
		}
	}
}

func confirmBodyRows(t *testing.T, m model, tc confirmCase) ([]string, string) {
	t.Helper()
	content := stripANSI(m.View().Content)
	lines := strings.Split(content, "\n")
	title := slices.IndexFunc(lines, func(line string) bool { return strings.Contains(line, tc.title[:10]) })
	action := slices.IndexFunc(lines, func(line string) bool { return strings.HasPrefix(line, "└─ ") })
	if title < 0 || action <= title {
		t.Fatalf("%s: confirm title or action row missing:\n%s", tc.name, content)
	}
	body := make([]string, 0, action-title-1)
	for _, row := range lines[title+1 : action] {
		body = append(body, strings.TrimLeft(strings.TrimRight(row, " "), "│ "))
	}
	return body, content
}

func checkConfirmBody(t *testing.T, tc confirmCase, width, height int) {
	t.Helper()
	m := newTestModel(new(fakeCaller), Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: width, Height: height})
	m.thread.id = "t1"
	tc.open(&m)
	var seen []string
	for range 20 {
		shown, hint := confirmFrameRows(t, m, tc, width, height)
		seen = mergeScrolled(seen, shown)
		if !hint {
			break
		}
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	}
	got := strings.Fields(strings.Join(seen, " "))
	want := append(slices.Clone(tc.lead), strings.Fields(tc.notice)...)
	if !slices.Equal(got, want) {
		t.Errorf("%s at %dx%d: body over the scroll reads %q, want %q", tc.name, width, height, got, want)
	}
}

func confirmFrameRows(t *testing.T, m model, tc confirmCase, width, height int) ([]string, bool) {
	t.Helper()
	bodyRows := min(6, height-4) - 2
	body, content := confirmBodyRows(t, m, tc)
	if len(body) != bodyRows {
		t.Fatalf("%s at %dx%d: %d body rows, want %d:\n%s", tc.name, width, height, len(body), bodyRows, content)
	}
	hint := strings.HasPrefix(body[len(body)-1], "↓ ")
	shown := body
	if hint {
		shown = body[:len(body)-1]
	}
	for _, row := range shown {
		if strings.HasSuffix(row, "...") || strings.Contains(row, "more lines") {
			t.Fatalf("%s at %dx%d: body row %q is cut:\n%s", tc.name, width, height, row, content)
		}
		if hint && row == "" {
			t.Fatalf("%s at %dx%d: the body does not fit while a body row stays empty:\n%s", tc.name, width, height, content)
		}
	}
	return shown, hint
}

func mergeScrolled(seen, shown []string) []string {
	for overlap := min(len(seen), len(shown)); overlap > 0; overlap-- {
		if slices.Equal(seen[len(seen)-overlap:], shown[:overlap]) {
			return append(seen, shown[overlap:]...)
		}
	}
	return append(seen, shown...)
}

func TestConfirmPanelCountsTheRowsBelow(t *testing.T) {
	t.Parallel()
	m := newTestModel(new(fakeCaller), Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 24, Height: 30})
	m.thread.id = "t1"
	m.openRewind("turn-9", "work")
	tc := confirmCase{name: "rewind", title: "Rewind to before this turn?"}
	body, content := confirmBodyRows(t, m, tc)
	const want = "↓ 2 more lines"
	if !strings.HasPrefix(body[len(body)-1], want) {
		t.Fatalf("last body row = %q, want it to start with %q:\n%s", body[len(body)-1], want, content)
	}
}
