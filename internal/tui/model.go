package tui

import (
	"time"

	tea "charm.land/bubbletea/v2"
)

func (m *model) scheduleShimmer() tea.Cmd {
	if m.input.shimmerActive || !m.shimmerVisible() {
		return nil
	}
	m.input.shimmerActive = true
	return tea.Tick(100*time.Millisecond, func(time.Time) tea.Msg { return shimmerMsg{} })
}

func (m *model) advanceShimmer() tea.Cmd {
	if !m.shimmerVisible() {
		m.input.shimmerActive = false
		return nil
	}
	m.input.phase = wrappedPhase(m.input.phase + 1.0/14.0)
	return tea.Tick(100*time.Millisecond, func(time.Time) tea.Msg { return shimmerMsg{} })
}

func (m *model) shimmerVisible() bool {
	return m.screen.focused && len(matchKeywords(m.input.composer.textView(), m.input.keywords)) > 0
}

func (m *model) composerPhase() float64 {
	if !m.screen.focused {
		return 0
	}
	return m.input.phase
}
