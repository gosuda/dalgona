package tui

import (
	"fmt"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func TestFollowupNoticeKeepsDockHeight(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Thread: rpc.Thread{ID: "t1"}}})
	m.queue = []string{"a", "b"}
	before := len(m.dockLines())
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Thread: rpc.Thread{ID: "t2"}}})
	if got := len(m.dockLines()); got != before {
		t.Fatalf("dock height after the follow-up notice = %d, want %d", got, before)
	}
}

func assertShown(t *testing.T, m model, text string) {
	t.Helper()
	if strings.Contains(strings.Join(strings.Fields(stripANSI(m.View().Content)), " "), text) {
		return
	}
	for _, entry := range m.out.entries {
		if strings.Contains(strings.Join(strings.Fields(stripANSI(entry.text)), " "), text) {
			return
		}
	}
	t.Fatalf("%q is neither in the view nor in a committed row:\n%s", text, m.View().Content)
}

func liveModel(t *testing.T, now *time.Time) model {
	t.Helper()
	caller := new(fakeCaller)
	m := newTestModel(caller, Options{}, nil)
	m.conn.clock = func() time.Time { return *now }
	m, _ = drive(t, m, notificationMsg{method: "turn/started", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}`)})
	return m
}

func cellCount(m model, text string) int {
	return m.screen.cells(text)
}

func TestLiveRowShowsWorkingTagAndElapsed(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	now = now.Add(12 * time.Second)
	row := stripANSI(m.liveRow())
	for _, want := range []string{"Working", "12s"} {
		if !strings.Contains(row, want) {
			t.Fatalf("live row %q lacks %q", row, want)
		}
	}
}

func TestLiveRowFieldsKeepCells(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	now = now.Add(9 * time.Second)
	before := stripANSI(m.liveRow())
	now = now.Add(time.Second)
	after := stripANSI(m.liveRow())
	if strings.Index(before, "Working") != strings.Index(after, "Working") {
		t.Fatalf("Working moved: %q then %q", before, after)
	}
	if cellCount(m, before) != cellCount(m, after) {
		t.Fatalf("live row width changed: %q then %q", before, after)
	}
}

func TestLiveRowFieldWidths(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	m.screen.width = 100
	for i := range 100 {
		m.live.items = append(m.live.items, liveItem{Item: rpc.Item{ID: fmt.Sprintf("c%d", i), Type: "commandExecution", Command: "x", Status: "inProgress"}})
	}
	m.queue = make([]string, 150)
	for range 120 {
		m.live.plan = append(m.live.plan, rpc.TurnPlanStep{Step: "s", Status: "pending"})
	}
	for i := range 100 {
		m.live.plan[i].Status = "completed"
	}
	m.thread.turnStartedAt = now.Add(-100 * time.Hour)
	for _, field := range []struct {
		name  string
		text  string
		cells int
		want  string
	}{
		{"calls", m.liveCalls(), liveCallsCells, "99+ run"},
		{"queued", m.liveQueued(), liveQueueCells, "99+ queued"},
		{"plan", m.livePlan(), livePlanCells, "99+/99+ plan"},
		{"elapsed", m.liveElapsed(), elapsedCells, "99h 59m"},
	} {
		if got := strings.TrimSpace(stripANSI(field.text)); got != field.want {
			t.Fatalf("%s field = %q, want %q", field.name, got, field.want)
		}
		if got := cellCount(m, field.text); got != field.cells {
			t.Fatalf("%s field is %d cells, want %d", field.name, got, field.cells)
		}
	}
}

func TestLiveRowBelowFortyColumns(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	m.screen.width = 30
	m.live.items = []liveItem{{Item: rpc.Item{ID: "a1", Type: "agentMessage", Text: strings.Repeat("tail ", 20)}}}
	for _, line := range m.dockLines() {
		if got := cellCount(m, line); got > 29 {
			t.Fatalf("dock line is %d cells at 30 columns: %q", got, line)
		}
	}
}

func TestNoticeShowsDuringTurn(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC)
	m := liveModel(t, &now)
	for _, r := range "/new" {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if !strings.Contains(stripANSI(m.liveRow()), "/new is unavailable") {
		t.Fatalf("refusal is not in the live row: %q", m.liveRow())
	}
	m, _ = drive(t, m, press('x', 0, "x"))
	if strings.Contains(stripANSI(m.liveRow()), "/new is unavailable") {
		t.Fatalf("the next key press left the refusal in the live row: %q", m.liveRow())
	}
}

func TestNoticeRowFitsOneCellShort(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m.screen.width = 40
	m.noticef("%s", strings.Repeat("n", 300))
	if got := cellCount(m, m.liveRow()); got > 39 {
		t.Fatalf("live row with a long notice is %d cells, want at most 39", got)
	}
}

func TestIdleLiveRowIsBlank(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	if got := strings.TrimSpace(stripANSI(m.liveRow())); got != "" {
		t.Fatalf("idle live row = %q, want blank", got)
	}
	m.thread.state = stateInterrupted
	if !strings.Contains(stripANSI(m.liveRow()), "Stopped") {
		t.Fatalf("interrupted live row lacks Stopped: %q", m.liveRow())
	}
}

func TestAwaitingInputShowsWaiting(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m.thread.state = "awaiting input"
	if !strings.Contains(stripANSI(m.liveRow()), "Waiting") {
		t.Fatalf("awaiting input live row lacks Waiting: %q", m.liveRow())
	}
}

func TestHintExpiresWithItsWindow(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	_ = m.setHint("Esc again to interrupt", "interrupt")
	stale := m.hint.id
	_ = m.setHint("Esc again to rewind", "rewind")
	m.expireHint(hintExpiredMsg{id: stale})
	if m.hint.text != "Esc again to rewind" {
		t.Fatalf("a stale expiry cleared the hint: %q", m.hint.text)
	}
	m.expireHint(hintExpiredMsg{id: m.hint.id})
	if m.hint.text != "" || m.hint.pending != "" {
		t.Fatalf("the current expiry kept the hint: %#v", m.hint)
	}
}

func TestMultiLineNoticeCutInLiveRowIsCommitted(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m.screen.width = 40
	m.noticef("%s", "a\nb")
	if cells := cellCount(m, m.liveText()); cells <= m.liveFreeWidth() {
		t.Fatalf("precondition: the live row shows the notice in %d cells, free width %d", cells, m.liveFreeWidth())
	}
	m.commitOverflowNotice()
	if len(m.out.entries) != 1 || m.out.entries[0].key != "note:0" {
		t.Fatalf("the cut notice was not committed as a row: %+v", m.out.entries)
	}
	if got := m.out.entries[0].text; got != "a\nb" {
		t.Fatalf("committed notice text = %q, want %q", got, "a\nb")
	}
}

func TestActivityNamesCommandAndFileChange(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m.live.items = []liveItem{{Item: rpc.Item{ID: "c1", Type: "commandExecution", Command: "go test ./...\nexit"}}}
	if got := m.activity(); got != "Running go test ./..." {
		t.Fatalf("command activity = %q, want %q", got, "Running go test ./...")
	}
	m.live.items = []liveItem{{Item: rpc.Item{ID: "c1", Type: "commandExecution", Command: "go test ./..."}, output: []byte("ok\nPASS\n")}}
	if got := m.activity(); got != "Running go test ./... · PASS" {
		t.Fatalf("command activity with output = %q, want %q", got, "Running go test ./... · PASS")
	}
	m.live.items = []liveItem{{Item: rpc.Item{ID: "f1", Type: "fileChange", Changes: []rpc.FileChange{{Path: "notes.md"}}}}}
	if got := m.activity(); got != "Editing notes.md" {
		t.Fatalf("file change activity = %q, want %q", got, "Editing notes.md")
	}
}

func TestHintUsesLiveRowAndAddsNoRow(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 40, Height: 8})
	before := len(m.dockLines())
	m.setHint("Esc again to interrupt", "interrupt")
	if got := len(m.dockLines()); got != before {
		t.Fatalf("dock height with a hint = %d, want %d", got, before)
	}
	m = rawUpdate(t, m, tea.WindowSizeMsg{Width: 100, Height: 8})
	assertShown(t, m, "Esc again to interrupt")
}
