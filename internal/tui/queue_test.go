package tui

import (
	"slices"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

func TestQueueCountIsFixedField(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	m.thread.id = "t1"
	m.thread.state = stateRunning
	m.notice = "free text"
	column := func(m model) int {
		row := ansi.Strip(m.liveRow())
		index := strings.Index(row, "free text")
		if index < 0 {
			t.Fatalf("live row %q has no free text", row)
		}
		return ansi.StringWidth(row[:index])
	}
	m.queue = []string{"one"}
	one := column(m)
	m.queue = append(m.queue, "two")
	if two := column(m); two != one {
		t.Fatalf("free text starts at column %d with two queued, want %d as with one", two, one)
	}
	m.queue = make([]string, 12)
	if twelve := column(m); twelve != one {
		t.Fatalf("free text starts at column %d with twelve queued, want %d as with one", twelve, one)
	}
	if band := ansi.Strip(m.bandRow()); !strings.Contains(band, "+ 12") {
		t.Fatalf("band %q does not show the queue count 12", band)
	}
}

func TestQueuePanelListsTexts(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	m.thread.id = "t1"
	m.thread.state = stateRunning
	m.queue = []string{"first line\nsecond line", "second"}
	m, _ = drive(t, m, press('q', tea.ModAlt, ""))
	view := ansi.Strip(strings.Join(m.dockLines(), "\n"))
	for _, want := range []string{"1. first line", "2. second"} {
		if !strings.Contains(view, want) {
			t.Fatalf("queue panel %q does not list %q", view, want)
		}
	}
	if rows := len(m.dockLines()); rows != 15 {
		t.Fatalf("dock has %d rows, want 15: input, 12 panel rows, live row and status band", rows)
	}
}

func TestAltUpDequeues(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m.queue = []string{"a", "b"}
	m, _ = drive(t, m, press(tea.KeyUp, tea.ModAlt, ""))
	if got := m.input.composer.Value(); got != "b" || !slices.Equal(m.queue, []string{"a"}) {
		t.Fatalf("alt+up moved %q into the composer and left queue %q; want b and [a]", got, m.queue)
	}
	m, _ = drive(t, m, press(tea.KeyUp, tea.ModAlt, ""))
	if !slices.Equal(m.queue, []string{"a"}) {
		t.Fatalf("alt+up with a non-empty composer changed the queue to %q", m.queue)
	}
	m.input.composer.SetValue("")
	m, _ = drive(t, m, press(tea.KeyUp, tea.ModAlt, ""))
	if got := m.input.composer.Value(); got != "a" || len(m.queue) != 0 {
		t.Fatalf("second alt+up moved %q; queue %q; want a and empty", got, m.queue)
	}
}
