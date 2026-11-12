package tui

import (
	"fmt"

	tea "charm.land/bubbletea/v2"
)

func (m model) Init() tea.Cmd {
	start := tea.Sequence(m.initialize(), m.loadKeywords(), m.startThread())
	return tea.Batch(m.input.composer.Focus(), start)
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	var command tea.Cmd
	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		command = m.updateKey(msg)
	case tea.WindowSizeMsg:
		m.screen.width = msg.Width
		m.input.composer.SetWidth(msg.Width)
	case tea.FocusMsg:
		m.screen.focused = true
		m.input.composer.focused = true
	case tea.BlurMsg:
		m.screen.focused = false
		m.input.composer.focused = false
	case notificationMsg, serverRequestMsg, resultMsg, shimmerMsg, disconnectedMsg:
		command = m.updateAsync(msg)
	default:
		command = m.updateComposer(msg)
	}
	if _, ok := msg.(shimmerMsg); !ok {
		command = tea.Batch(command, m.scheduleShimmer())
	}
	return m, command
}

func (m *model) updateAsync(msg tea.Msg) tea.Cmd {
	switch msg := msg.(type) {
	case notificationMsg:
		return m.applyNotification(msg)
	case serverRequestMsg:
		m.applyRequest(msg)
	case resultMsg:
		return m.applyResult(msg)
	case shimmerMsg:
		return m.advanceShimmer()
	case disconnectedMsg:
		m.setState(stateDisconnected)
		m.notice = fmt.Sprintf("connection lost: %v; restart the TUI, then %s", msg.err, m.resumeHint())
	}
	return nil
}

func (m *model) updateKey(msg tea.KeyPressMsg) tea.Cmd {
	if msg.String() == "ctrl+c" {
		return m.confirmExit()
	}
	m.ctrlCArmed = false
	if len(m.approvals) > 0 {
		return m.answerApprovalKey(msg.String())
	}
	if m.selector != nil {
		return m.selectKey(msg.String())
	}
	switch msg.String() {
	case "enter":
		return m.submitPrompt()
	case "alt+enter":
		return m.queueFollowup()
	case "esc":
		return m.interrupt()
	default:
		return m.updateComposer(msg)
	}
}

func (m *model) confirmExit() tea.Cmd {
	if m.ctrlCArmed {
		return tea.Quit
	}
	m.ctrlCArmed = true
	m.notice = "press Ctrl+C again to exit; Esc interrupts the active turn"
	return nil
}

func (m *model) updateComposer(msg tea.Msg) tea.Cmd {
	updated, command := m.input.composer.Update(msg)
	m.input.composer = updated
	return command
}

func (m model) View() tea.View {
	view := tea.NewView(m.renderView())
	view.AltScreen = false
	view.ReportFocus = true
	return view
}
