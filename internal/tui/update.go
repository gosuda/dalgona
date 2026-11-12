package tui

import (
	"fmt"
	"strings"

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
	case notificationMsg, serverRequestMsg, resultMsg, disconnectedMsg, debounceMsg, searchResultMsg, shellResultMsg, imageMsg:
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
	quit := m.finishQuit()
	suspend := m.finishSuspend()
	return m, tea.Batch(command, tick, settle, printCommand, quit, suspend)
}

func (m *model) updateTerminal(msg tea.Msg) (tea.Cmd, bool) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.screen.width = msg.Width
		m.screen.height = msg.Height
		m.input.composer.SetWidth(msg.Width)
		m.fitReview()
	case tea.FocusMsg:
		m.screen.focused = true
		m.input.composer.focused = true
	case tea.BlurMsg:
		m.screen.focused = false
		m.input.composer.focused = false
	case tea.MouseWheelMsg:
		m.scrollReview(msg)
	case tea.ModeReportMsg:
		m.applyModeReport(msg)
	default:
		if m.updateFrame(msg) {
			return nil, true
		}
		return m.updateColour(msg)
	}
	return nil, true
}

func (m *model) updateFrame(msg tea.Msg) bool {
	switch msg := msg.(type) {
	case printedMsg:
		m.out.printing = false
		m.out.started = true
	case settledMsg:
		if msg.n == m.out.settleN {
			m.out.settling = false
		}
	case tea.ResumeMsg:
		m.leaving = false
	default:
		return false
	}
	return true
}

func (m *model) updateAsync(msg tea.Msg) tea.Cmd {
	switch msg := msg.(type) {
	case debounceMsg:
		return m.applyDebounce(msg)
	case searchResultMsg:
		m.applySearch(msg)
		return nil
	case notificationMsg:
		return m.applyNotification(msg)
	case serverRequestMsg:
		m.applyRequest(msg)
	case resultMsg:
		return m.applyResultMessage(msg)
	case shellResultMsg:
		m.applyShell(msg)
	case disconnectedMsg:
		m.setState(stateDisconnected)
		m.errorf("connection lost: %v; restart the TUI, then %s", msg.err, m.resumeHint())
	case imageMsg:
		m.applyImage(msg)
	}
	return nil
}

func (m *model) applyResultMessage(msg resultMsg) tea.Cmd {
	if msg.op == "keywordMatch" {
		m.applyKeywordMatch(msg)
		return nil
	}
	return m.applyResult(msg)
}

func (m *model) overlayKey(msg tea.KeyPressMsg) (tea.Cmd, bool) {
	key := msg.String()
	switch {
	case m.review != nil:
		return m.reviewKey(msg), true
	case key == "ctrl+o":
		m.openReview()
		return nil, true
	case len(m.approvals) > 0:
		return m.answerApprovalKey(key), true
	case m.confirm != nil:
		return m.confirmKey(key), true
	case m.list != nil:
		return m.listKey(msg), true
	case m.settings != nil:
		return m.settingsKey(msg), true
	case m.login != nil:
		return m.loginKey(msg), true
	}
	return nil, false
}

func (m *model) composerKey(msg tea.KeyPressMsg, key string) tea.Cmd {
	switch key {
	case "up":
		if m.recallOlder() {
			return nil
		}
	case "down":
		if m.recallNewer() {
			return nil
		}
	}
	return m.updateComposer(msg)
}

func (m *model) keyCommand(msg tea.KeyPressMsg) tea.Cmd {
	m.noticef("")
	key := msg.String()
	if command, handled := m.guardKey(key); handled {
		return command
	}
	if command, handled := m.overlayKey(msg); handled {
		return command
	}
	if command, handled := m.followupKey(msg); handled {
		return command
	}
	if command, handled := m.shellKey(key, msg); handled {
		return command
	}
	if command, handled := m.popupKey(msg); handled {
		return command
	}
	return m.promptKey(msg, key)
}

func (m *model) promptKey(msg tea.KeyPressMsg, key string) tea.Cmd {
	switch key {
	case "enter":
		if m.input.composer.continueLine() {
			return nil
		}
		return m.submitPrompt()
	case "alt+enter":
		return m.queueFollowup()
	case "ctrl+v":
		return m.clipboardImage()
	case "esc":
		return m.escape()
	default:
		return m.composerKey(msg, key)
	}
}

func (m *model) updateKey(msg tea.KeyPressMsg) tea.Cmd {
	command := m.keyCommand(msg)
	return tea.Batch(command, m.syncPopup(false))
}

func (m *model) followupKey(msg tea.KeyPressMsg) (tea.Cmd, bool) {
	switch msg.String() {
	case "alt+q":
		m.openQueue()
		return nil, true
	case "alt+up":
		if m.input.composer.Value() == "" && len(m.queue) > 0 {
			return m.dequeueFollowup(), true
		}
	}
	return nil, false
}

func (m *model) guardKey(key string) (tea.Cmd, bool) {
	switch key {
	case "ctrl+c":
		return m.exitKey(), true
	case "ctrl+z":
		m.suspend()
		return nil, true
	}
	if key != "esc" {
		m.clearGuard()
	}
	return nil, false
}

func (m *model) exitKey() tea.Cmd {
	if m.hint.pending == "exit" {
		m.quit()
		return nil
	}
	if draft := m.input.composer.Value(); strings.TrimSpace(draft) != "" {
		m.input.history.add(draft, "")
		m.input.composer.SetValue("")
		return m.setHint("Draft saved to history · Ctrl+C again to exit", "exit")
	}
	return m.setHint("Ctrl+C again to exit", "exit")
}

func (m *model) recallOlder() bool {
	if !m.input.composer.onFirstRow(&m.screen) {
		return false
	}
	text, ok := m.input.history.older(m.input.composer.Value())
	if ok {
		m.input.composer.SetValue(text)
	}
	return ok
}

func (m *model) recallNewer() bool {
	if !m.input.composer.onLastRow(&m.screen) {
		return false
	}
	text, ok := m.input.history.newer()
	if ok {
		m.input.composer.SetValue(text)
	}
	return ok
}

func (m *model) escape() tea.Cmd {
	if !m.turnLive() {
		if m.thread.state == stateIdle {
			return m.idleEscape()
		}
		return m.interrupt()
	}
	if _, waiting := m.retryRemaining(); waiting || m.hint.pending == "interrupt" {
		m.clearHint()
		return m.interrupt()
	}
	return m.setHint("Esc again to interrupt", "interrupt")
}

func (m *model) idleEscape() tea.Cmd {
	if m.input.composer.Value() != "" {
		return m.interrupt()
	}
	if m.thread.id == "" {
		return nil
	}
	if m.hint.pending == "rewind" {
		m.clearHint()
		return m.rewindCommand()
	}
	return m.setHint("Esc again to rewind", "rewind")
}

func (m *model) clearGuard() {
	if m.hint.pending != "" {
		m.clearHint()
	}
}

func (m *model) quit() {
	m.closeReview()
	m.leaving = true
	m.quitting = true
}

func (m *model) finishQuit() tea.Cmd {
	if !m.quitting {
		return nil
	}
	if m.screen.height == 0 || (!m.out.printing && !m.out.settling && len(m.out.pending) == 0) {
		return tea.Quit
	}
	return nil
}

func (m *model) suspend() {
	m.closeReview()
	m.leaving = true
	m.suspending = true
}

func (m *model) finishSuspend() tea.Cmd {
	if !m.suspending {
		return nil
	}
	if m.screen.height == 0 || (!m.out.printing && len(m.out.pending) == 0) {
		m.suspending = false
		return tea.Suspend
	}
	return nil
}

func (m *model) updateComposer(msg tea.Msg) tea.Cmd {
	if paste, ok := msg.(tea.PasteMsg); ok && (m.list != nil || m.loginTyping()) {
		m.input.field.pasteLine(paste.Content)
		return nil
	}
	if paste, ok := msg.(tea.PasteMsg); ok && len(paste.Content) > pasteLimit {
		m.noticef("Paste is %d bytes, over the 1 MiB limit; save it to a file and mention it with @path", len(paste.Content))
		return nil
	}
	var imageCmd tea.Cmd
	if paste, ok := msg.(tea.PasteMsg); ok && m.textMode() == inputPrompt {
		imageCmd = m.imagePasteCmd(sanitizeText(paste.Content))
	}
	updated, command := m.input.composer.Update(&m.screen, msg)
	m.input.composer = updated
	return tea.Batch(command, m.syncPopup(typedText(msg)), imageCmd)
}

func (m model) View() tea.View {
	if m.out.printing {
		return m.out.frozen
	}
	if m.leaving {
		view := tea.NewView(" ")
		view.ProgramStatus = &tea.ProgramStatus{State: tea.ProgramStateIdle}
		return view
	}
	if !m.out.started {
		view := tea.NewView("")
		view.ReportFocus = true
		return view
	}
	if m.review != nil {
		view := tea.NewView(m.screen.emit(m.reviewView()))
		view.AltScreen = true
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
