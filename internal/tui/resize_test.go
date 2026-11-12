package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func TestNarrowingKeepsComposerRowsInsideWindow(t *testing.T) {
	t.Parallel()
	m := newTestModel(new(fakeCaller), Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	m = typeText(t, m, strings.Repeat("x", 130))
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 24})
	assertFrame(t, m.dockLines(), 40, 24)
}
