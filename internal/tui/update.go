package tui

import (
	"fmt"

	"charm.land/bubbles/v2/spinner"
	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

func (m model) Init() tea.Cmd {
	start := tea.Sequence(m.initialize(), m.readSettings(), m.loadKeywords(), m.loadLevels(), m.startThread())
	return tea.Batch(m.input.composer.Focus(), tea.RequestBackgroundColor, start)
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	var command tea.Cmd
	before := m.input.composer.Value()
	notice := m.notice
	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		command = m.updateKey(msg)
	case notificationMsg, serverRequestMsg, resultMsg, disconnectedMsg:
		command = m.updateAsync(msg)
	case spinner.TickMsg:
		command = m.updateTick(msg)
	case hintExpiredMsg:
		m.expireHint(msg)
	default:
		var handled bool
		command, handled = m.updateTerminal(msg)
		if !handled {
			command = m.updateComposer(msg)
		}
	}
	if m.input.composer.Value() != before {
		command = tea.Batch(command, m.matchKeywords())
	}
	if m.notice != notice {
		m.commitOverflowNotice()
	}
	settle := m.settle()
	printCommand := m.drain()
	tick := m.scheduleTick()
	return m, tea.Batch(command, tick, settle, printCommand)
}

func (m *model) updateTerminal(msg tea.Msg) (tea.Cmd, bool) {
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
		return m.updateColour(msg)
	}
	return nil, true
}

func (m *model) updateAsync(msg tea.Msg) tea.Cmd {
	switch msg := msg.(type) {
	case notificationMsg:
		return m.applyNotification(msg)
	case serverRequestMsg:
		m.applyRequest(msg)
	case resultMsg:
		if msg.op == "keywordMatch" {
			m.applyKeywordMatch(msg)
			return nil
		}
		return m.applyResult(msg)
	case disconnectedMsg:
		m.setState(stateDisconnected)
		m.errorf("connection lost: %v; restart the TUI, then %s", msg.err, m.resumeHint())
	}
	return nil
}

func (m *model) updateKey(msg tea.KeyPressMsg) tea.Cmd {
	m.noticef("")
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
	view := m.dockView()
	view.Content = m.screen.emit(view.Content)
	return view
}

func (m *model) noticef(format string, args ...any) {
	m.notice = sanitizeText(fmt.Sprintf(format, args...))
	m.noticeError = false
}

func (m *model) errorf(format string, args ...any) {
	m.noticef(format, args...)
	m.noticeError = true
}

func (m *model) commitOverflowNotice() {
	if m.notice == "" {
		return
	}
	width := terminalWidth(m.screen.width)
	if m.screen.cells(m.noticeLine()) <= m.liveFreeWidth() {
		return
	}
	m.commit(fmt.Sprintf("note:%d", m.noticeN), m.screen.wrap(m.notice, width))
	m.noticeN++
}

func (m *model) applyModeReport(msg tea.ModeReportMsg) {
	if msg.Mode != ansi.ModeUnicodeCore {
		return
	}
	if msg.Value == ansi.ModeReset || msg.Value == ansi.ModeSet || msg.Value == ansi.ModePermanentlySet {
		m.screen.method = ansi.GraphemeWidth
	}
}
