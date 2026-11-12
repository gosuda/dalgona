package tui

import (
	"fmt"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
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
	case notificationMsg, serverRequestMsg, resultMsg, shimmerMsg, disconnectedMsg:
		command = m.updateAsync(msg)
	default:
		if !m.updateTerminal(msg) {
			command = m.updateComposer(msg)
		}
	}
	if _, ok := msg.(shimmerMsg); !ok {
		command = tea.Batch(command, m.scheduleShimmer())
	}
	settle := m.settle()
	printCommand := m.drain()
	return m, tea.Batch(command, settle, printCommand)
}

func (m *model) updateTerminal(msg tea.Msg) bool {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.screen.width = msg.Width
		m.screen.height = msg.Height
		m.input.composer.SetWidth(msg.Width)
	case tea.FocusMsg:
		m.screen.focused = true
		m.input.composer.focused = true
	case tea.BlurMsg:
		m.screen.focused = false
		m.input.composer.focused = false
	case tea.ModeReportMsg:
		m.applyModeReport(msg)
	case printedMsg:
		m.out.printing = false
		m.out.started = true
	case settledMsg:
		if msg.n == m.out.settleN {
			m.out.settling = false
		}
	default:
		return false
	}
	return true
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
		m.errorf("connection lost: %v; restart the TUI, then %s", msg.err, m.resumeHint())
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
	if m.confirm != nil {
		return m.confirmKey(msg.String())
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
	m.noticef("press Ctrl+C again to exit; Esc interrupts the active turn")
	return nil
}

func (m *model) updateComposer(msg tea.Msg) tea.Cmd {
	updated, command := m.input.composer.Update(msg)
	m.input.composer = updated
	return command
}

func (m model) View() tea.View {
	if m.out.printing {
		return m.out.frozen
	}
	if !m.out.started {
		view := tea.NewView("")
		view.ReportFocus = true
		return view
	}
	return m.dockView()
}

func (m *model) noticef(format string, args ...any) {
	m.notice = sanitizeText(fmt.Sprintf(format, args...))
	m.noticeError = false
}

func (m *model) errorf(format string, args ...any) {
	m.noticef(format, args...)
	m.noticeError = true
}

func (m *model) applyModeReport(msg tea.ModeReportMsg) {
	if msg.Mode != ansi.ModeUnicodeCore {
		return
	}
	if msg.Value == ansi.ModeReset || msg.Value == ansi.ModeSet || msg.Value == ansi.ModePermanentlySet {
		m.screen.method = ansi.GraphemeWidth
	}
}
