package tui

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

type rejectingCaller struct {
	fakeCaller
	reject string
}

func (f *rejectingCaller) Call(ctx context.Context, method string, params, result any) error {
	if method == f.reject {
		f.calls = append(f.calls, callRecord{method: method, params: params})
		return errors.New("disk full")
	}
	return f.fakeCaller.Call(ctx, method, params, result)
}

func listModels() []rpc.Model {
	efforts := []rpc.ReasoningEffortOption{{ReasoningEffort: "low"}, {ReasoningEffort: "high"}}
	return []rpc.Model{
		{ID: "openai/gpt-test", DefaultReasoningEffort: "low", SupportedReasoningEfforts: efforts, Presets: []string{"default"}},
		{ID: "openai/gpt-mini", DefaultReasoningEffort: "low", SupportedReasoningEfforts: efforts, Presets: []string{"smol"}},
		{ID: "anthropic/claude", DefaultReasoningEffort: "low", SupportedReasoningEfforts: efforts},
	}
}

func openModelPicker(t *testing.T, client caller) model {
	t.Helper()
	m := newTestModel(client, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, resultMsg{op: "models", value: &rpc.ModelListResponse{Data: listModels()}})
	if m.list == nil {
		t.Fatal("model picker did not open")
	}
	return m
}

func runPickCommand(t *testing.T, m model, command tea.Cmd) model {
	t.Helper()
	if command == nil {
		return m
	}
	switch msg := command().(type) {
	case tea.BatchMsg:
		for _, next := range msg {
			m = runPickCommand(t, m, next)
		}
	case resultMsg:
		current := playTerminal(m)
		updated, next := current.Update(msg)
		m = updated.(model)
		m = runPickCommand(t, m, next)
	}
	return m
}

func configWriteKeys(client *fakeCaller) []string {
	var keys []string
	for _, call := range client.calls {
		if params, ok := call.params.(rpc.ConfigValueWriteParams); ok && call.method == "config/value/write" {
			keys = append(keys, params.KeyPath)
		}
	}
	return keys
}

func TestModelPickerShowsPresets(t *testing.T) {
	t.Parallel()
	m := openModelPicker(t, &fakeCaller{})
	assertContains(t, m.View().Content, "default · openai/gpt-test", "smol · openai/gpt-mini")
}

func TestModelPickerFilterAndEffort(t *testing.T) {
	t.Parallel()
	client := &fakeCaller{}
	m := openModelPicker(t, client)
	for _, r := range "gpt" {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	m, _ = drive(t, m, press(tea.KeyTab, 0, ""))
	m, command := drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	m = runPickCommand(t, m, command)
	want := []string{"model", "model_reasoning_effort"}
	if got := configWriteKeys(client); strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("config writes = %v, want %v", got, want)
	}
	assertModelPickerWriteValues(t, client)
	if m.list != nil {
		t.Fatal("model picker stayed open after both writes succeeded")
	}
	unchanged := &fakeCaller{}
	m = openModelPicker(t, unchanged)
	m, command = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	runPickCommand(t, m, command)
	if got := configWriteKeys(unchanged); strings.Join(got, ",") != "model" {
		t.Fatalf("config writes without effort change = %v, want [model]", got)
	}
}

func assertModelPickerWriteValues(t *testing.T, client *fakeCaller) {
	t.Helper()
	for _, call := range client.calls {
		params, ok := call.params.(rpc.ConfigValueWriteParams)
		if !ok || call.method != "config/value/write" {
			continue
		}
		wantValue := "openai/gpt-test"
		if params.KeyPath == "model_reasoning_effort" {
			wantValue = "high"
		}
		if params.Value != wantValue {
			t.Fatalf("config write %s value = %v, want %s", params.KeyPath, params.Value, wantValue)
		}
	}
}

func TestPickerSaveErrorStaysOpen(t *testing.T) {
	t.Parallel()
	client := &rejectingCaller{reject: "config/value/write"}
	m := openModelPicker(t, client)
	m, command := drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	m = runPickCommand(t, m, command)
	if m.list == nil {
		t.Fatal("model picker closed after a failed save")
	}
	assertContains(t, m.View().Content, "Could not save: disk full")
}

func TestPickerEscClearsFilterThenCloses(t *testing.T) {
	t.Parallel()
	m := openModelPicker(t, &fakeCaller{})
	m, _ = drive(t, m, press('x', 0, "x"))
	if got := m.input.field.value(); got != "x" {
		t.Fatalf("filter = %q, want x", got)
	}
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
	if m.list == nil || m.input.field.value() != "" {
		t.Fatal("first Esc must clear the filter and keep the panel open")
	}
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
	if m.list != nil {
		t.Fatal("second Esc must close the panel")
	}
}

func TestResumePickerFilters(t *testing.T) {
	t.Parallel()
	m := newTestModel(&fakeCaller{}, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	threads := rpc.ThreadListResponse{Data: []rpc.Thread{{ID: "t-demo", Name: "demo session"}, {ID: "t-other", Name: "other work"}}}
	m, _ = drive(t, m, resultMsg{op: "threads", value: &threads})
	for _, r := range "demo" {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	view := m.View().Content
	assertContains(t, view, "demo session")
	if strings.Contains(view, "other work") {
		t.Fatal("filter kept a row that does not match demo")
	}
}

func TestListDockSteady(t *testing.T) {
	t.Parallel()
	m := openModelPicker(t, &fakeCaller{})
	if got := len(m.dockLines()); got != 15 {
		t.Fatalf("dock with a list open has %d lines, want 15", got)
	}
	keys := make([]tea.KeyPressMsg, 0, 100)
	for range 50 {
		keys = append(keys, press('a', 0, "a"))
	}
	for range 50 {
		keys = append(keys, tea.KeyPressMsg{Code: tea.KeyDown})
	}
	assertSteadyDock(t, m, keys)
}

func TestListOpenAndCloseChangeHeightOnce(t *testing.T) {
	t.Parallel()
	m := newTestModel(&fakeCaller{}, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	closed := len(m.dockLines())
	m, _ = drive(t, m, resultMsg{op: "threads", value: &rpc.ThreadListResponse{Data: []rpc.Thread{{ID: "t1"}}}})
	open := len(m.dockLines())
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
	after := len(m.dockLines())
	if closed != 3 || open != 15 || after != 3 {
		t.Fatalf("dock heights closed %d, open %d, closed again %d; want 3, 15, 3", closed, open, after)
	}
}

func TestApprovalOverListShowsPromptLine(t *testing.T) {
	t.Parallel()
	m := openModelPicker(t, &fakeCaller{})
	m.approvals = []approval{{threadID: "t1", command: "echo hi"}}
	if mode := m.textMode(); mode == inputFilter {
		t.Fatalf("text mode is %v with an approval on top of a list, want the prompt", mode)
	}
	if line := m.dockLines()[0]; strings.Contains(line, "filter: ") {
		t.Fatalf("input line shows the list filter under an approval: %q", line)
	}
}

func TestResumeRowDates(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 15, 12, 0, 0, 0, time.UTC)
	m := newTestModel(&fakeCaller{}, Options{})
	m.conn.clock = func() time.Time { return now }
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	threads := rpc.ThreadListResponse{Data: []rpc.Thread{
		{ID: "t1", Name: "Fix parser", Status: rpc.ThreadStatus{Type: "active"}, UpdatedAt: time.Date(2026, 5, 3, 9, 0, 0, 0, time.UTC).Unix()},
		{ID: "t2", Name: "Add tests", Status: rpc.ThreadStatus{Type: "idle"}, CreatedAt: time.Date(2025, 11, 20, 9, 0, 0, 0, time.UTC).Unix()},
		{ID: "t3", Name: "No dates", Status: rpc.ThreadStatus{Type: "idle"}},
		{ID: "t4", Name: "Created fallback", Status: rpc.ThreadStatus{Type: "idle"}, CreatedAt: time.Date(2026, 1, 2, 9, 0, 0, 0, time.UTC).Unix()},
	}}
	m, _ = drive(t, m, resultMsg{op: "threads", value: &threads})
	view := m.View().Content
	assertContains(t, view, "05-03", "2025-11", "01-02")
	line1 := m.listLine(m.list.items[0], false)
	line2 := m.listLine(m.list.items[1], false)
	line3 := m.listLine(m.list.items[2], false)
	if strings.Contains(line3, "-") {
		t.Fatalf("row without timestamps shows a date: %q", line3)
	}
	if !strings.Contains(stripANSI(line1), "  05-03  ") {
		t.Fatalf("current-year date is not right aligned in its field: %q", line1)
	}
	if !strings.Contains(stripANSI(line2), "2025-11  ") {
		t.Fatalf("earlier-year date does not fill its field: %q", line2)
	}
	detailOffset := func(line string) int { return m.screen.cells(line) - m.screen.cells("idle") }
	if detailOffset(line2) != detailOffset(line3) {
		t.Fatalf("blank date column moved the status: %q vs %q", line2, line3)
	}
	for _, width := range []int{40, 100} {
		wm, _ := drive(t, m, tea.WindowSizeMsg{Width: width, Height: 24})
		wview := wm.View().Content
		assertContains(t, wview, "05-03", "2025-11")
		m = wm
	}
}
