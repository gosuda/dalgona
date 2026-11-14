package tui

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func longList(n int) *listPanel {
	items := make([]listItem, n)
	for i := range items {
		items[i] = listItem{id: fmt.Sprint(i), label: fmt.Sprintf("item %03d", i)}
	}
	return &listPanel{kind: "resume", title: "resume a thread", items: items, efforts: map[string]effortPick{}}
}

func assertMouseMode(t *testing.T, m model, want tea.MouseMode, when string) {
	t.Helper()
	if got := m.View().MouseMode; got != want {
		t.Fatalf("%s: mouse mode %v, want %v", when, got, want)
	}
}

func TestMouseModeOnlyWithOverlay(t *testing.T) {
	t.Parallel()
	m := newTestModel(&fakeCaller{}, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, resultMsg{op: "threads", value: &rpc.ThreadListResponse{Data: []rpc.Thread{{ID: "t1"}}}})
	assertMouseMode(t, m, tea.MouseModeCellMotion, "resume list open")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
	assertMouseMode(t, m, tea.MouseModeNone, "resume list closed")

	m, _ = drive(t, m, fileApprovalRequest(make(chan requestAnswer, 1)))
	assertMouseMode(t, m, tea.MouseModeNone, "approval card open")
	m, _ = drive(t, m, press('n', 0, "n"))
	assertMouseMode(t, m, tea.MouseModeNone, "approval card closed")

	review := reviewModel(t, 80, 300)
	review = rawUpdate(t, review, press('o', tea.ModCtrl, ""))
	assertMouseMode(t, review, tea.MouseModeCellMotion, "review open")
	review = rawUpdate(t, review, tea.KeyPressMsg{Code: tea.KeyEscape})
	assertMouseMode(t, review, tea.MouseModeNone, "review closed")
}

func TestWheelScrollsPicker(t *testing.T) {
	t.Parallel()
	m := newTestModel(&fakeCaller{}, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m.openList(longList(300))
	m, _ = drive(t, m, tea.MouseWheelMsg{Button: tea.MouseWheelDown})
	view := m.View().Content
	if !strings.Contains(view, "item 003") || strings.Contains(view, "item 000") {
		t.Fatalf("wheel down did not move the window three rows:\n%s", view)
	}
	if m.list.cursor != 3 {
		t.Fatalf("cursor = %d, want 3, the nearest visible row", m.list.cursor)
	}
}

func TestClickSelectsNothing(t *testing.T) {
	t.Parallel()
	m := newTestModel(&fakeCaller{}, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m.openList(longList(300))
	before := *m.list
	m, _ = drive(t, m, tea.MouseClickMsg{Button: tea.MouseLeft, X: 2, Y: 3})
	if m.list.cursor != before.cursor {
		t.Fatalf("click moved the cursor from %d to %d", before.cursor, m.list.cursor)
	}
	if !strings.Contains(m.View().Content, "item 000") {
		t.Fatal("click moved the window")
	}
}
